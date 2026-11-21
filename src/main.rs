use std::{
    io::{self, IsTerminal},
    path::{Path, PathBuf},
    process::ExitCode,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::RecvTimeoutError,
    },
    time::{Duration, Instant},
};

use clap::Parser;
use crossterm::{
    cursor::{Hide, Show},
    event::{self, Event as TerminalEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    execute,
    terminal::{self, EnterAlternateScreen, LeaveAlternateScreen},
};
use jscope::{
    journal::{Frame, sanitize},
    source::{DEFAULT_BACKLOG, JournalFilters, Source, SourceMessage},
    state::{Grouping, State},
    ui,
};
use ratatui::{Terminal, backend::CrosstermBackend};
use signal_hook::consts::signal::{SIGHUP, SIGINT, SIGTERM};

const FRAME_BATCH: usize = 256;
const POLL_INTERVAL: Duration = Duration::from_millis(40);
const IDLE_REDRAW: Duration = Duration::from_millis(250);
const MAX_SEARCH_BYTES: usize = 1024;

#[derive(Debug, Parser)]
#[command(
    version,
    about = "Watch the systemd journal and see which services got noisy"
)]
struct Args {
    /// Read journalctl JSON from a file or from standard input (-).
    #[arg(long, value_name = "FILE")]
    input: Option<PathBuf>,

    /// Maximum number of matching journal entries to load before following.
    #[arg(long, default_value_t = DEFAULT_BACKLOG, conflicts_with = "input")]
    backlog: u32,

    /// Match a journalctl unit name or pattern. Repeat to add more units.
    #[arg(short, long, value_name = "UNIT", conflicts_with = "input")]
    unit: Vec<String>,

    /// Match a journalctl user unit name or pattern. Repeat to add more user units.
    #[arg(long, value_name = "UNIT", conflicts_with = "input")]
    user_unit: Vec<String>,

    /// Match the current boot or a journalctl boot ID or offset.
    #[arg(
        short,
        long,
        value_name = "BOOT",
        num_args = 0..=1,
        default_missing_value = "0",
        allow_negative_numbers = true,
        conflicts_with = "input"
    )]
    boot: Option<String>,

    /// Match a journalctl priority name, number, or inclusive range.
    #[arg(short, long, value_name = "PRIORITY", conflicts_with = "input")]
    priority: Option<String>,

    /// Start with lower severities muted only for these exact journal unit names. Repeatable.
    #[arg(long, value_name = "UNIT", value_parser = clap::builder::NonEmptyStringValueParser::new())]
    mute_unit: Vec<String>,

    /// Group only consecutive exact repeats. By default, nearby routine repeats can span other units.
    #[arg(long)]
    adjacent_only: bool,

    /// Disable terminal colors.
    #[arg(long)]
    no_color: bool,
}

impl Args {
    fn initial_state(&self) -> io::Result<State> {
        let mut state = State::default();
        if self.adjacent_only {
            state.set_grouping(Grouping::Adjacent);
        }
        for unit in &self.mute_unit {
            state
                .set_unit_muted(&sanitize(unit), true)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
        }
        Ok(state)
    }
}

struct Screen;

impl Screen {
    fn enter() -> io::Result<Self> {
        terminal::enable_raw_mode()?;
        let screen = Self;
        execute!(io::stdout(), EnterAlternateScreen, Hide)?;
        Ok(screen)
    }
}

impl Drop for Screen {
    fn drop(&mut self) {
        let _ = execute!(io::stdout(), Show, LeaveAlternateScreen);
        let _ = terminal::disable_raw_mode();
    }
}

struct InputStatus {
    label: &'static str,
    end_label: &'static str,
    notice: Option<String>,
    malformed: u64,
    oversized: u64,
    ended: bool,
    failed: bool,
}

impl InputStatus {
    fn new(replay: bool) -> Self {
        let (label, end_label) = if replay {
            ("replay", "replay complete")
        } else {
            ("journalctl", "journalctl ended")
        };
        Self {
            label,
            end_label,
            notice: None,
            malformed: 0,
            oversized: 0,
            ended: false,
            failed: false,
        }
    }

    fn accept(&mut self, message: SourceMessage, state: &mut State, now: u64) {
        match message {
            SourceMessage::Frame(Frame::Event(event)) => state.ingest(event, now),
            SourceMessage::Frame(Frame::Malformed) => {
                self.malformed = self.malformed.saturating_add(1);
            }
            SourceMessage::Frame(Frame::Oversized) => {
                self.oversized = self.oversized.saturating_add(1);
            }
            SourceMessage::Diagnostic(notice) => self.notice = Some(notice),
            SourceMessage::Finished => self.ended = true,
            SourceMessage::Error(error) => {
                self.ended = true;
                self.failed = true;
                self.notice = Some(match self.notice.take() {
                    Some(notice) => format!("{error} · {notice}"),
                    None => error,
                });
            }
        }
    }

    fn label(&self) -> &'static str {
        if self.failed {
            "source error"
        } else if self.ended {
            self.end_label
        } else {
            self.label
        }
    }
}

#[derive(Default)]
struct Controls {
    detail_scroll_limit: usize,
    service_focus: bool,
    search: String,
    previous_search: Option<String>,
    notice: Option<String>,
}

impl Controls {
    fn search_input(&self) -> Option<&str> {
        self.previous_search.as_ref().map(|_| self.search.as_str())
    }

    fn key(&mut self, key: KeyEvent, state: &mut State) -> bool {
        if key.kind == KeyEventKind::Release {
            return true;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return false;
        }
        self.notice = None;
        if self.previous_search.is_some() {
            self.edit_search(key, state);
            return true;
        }
        if key.code == KeyCode::Char('q') {
            return false;
        }
        if state.spike().is_some() {
            self.spike_key(key.code, state);
            return true;
        }
        match key.code {
            KeyCode::Esc => return false,
            KeyCode::Tab => self.service_focus = !self.service_focus,
            KeyCode::Up | KeyCode::Down => {
                let delta = if key.code == KeyCode::Up { -1 } else { 1 };
                if self.service_focus {
                    state.move_unit(delta);
                } else {
                    state.move_row(delta);
                }
            }
            KeyCode::PageUp => state.scroll_details(-6, self.detail_scroll_limit),
            KeyCode::PageDown => state.scroll_details(6, self.detail_scroll_limit),
            KeyCode::Enter if self.service_focus => {
                self.notice =
                    (!state.begin_spike()).then(|| "No service has activity to inspect".into());
            }
            KeyCode::Enter => state.toggle_expanded(),
            KeyCode::Char('p') if self.service_focus => {
                self.notice = state.toggle_pin().err().map(String::from);
            }
            KeyCode::Char(key @ ('[' | ']')) if !self.service_focus => {
                let direction = if key == '[' { -1 } else { 1 };
                self.notice = state.hop_spike(direction).err().map(String::from);
            }
            KeyCode::Char('s') => state.toggle_solo(),
            KeyCode::Char('m') => state.toggle_mute(),
            KeyCode::Char('M') => {
                self.notice = state.toggle_unit_mute().err().map(|err| err.to_string());
            }
            KeyCode::Char('/') => self.previous_search = Some(self.search.clone()),
            KeyCode::Char(' ') => state.toggle_pause(),
            KeyCode::Char('g') | KeyCode::End => state.jump_live(),
            _ => {}
        }
        true
    }

    fn spike_key(&mut self, code: KeyCode, state: &mut State) {
        match code {
            KeyCode::Left => state.move_spike(-1),
            KeyCode::Right => state.move_spike(1),
            KeyCode::Up | KeyCode::Down => state.switch_spike_unit(),
            KeyCode::Enter => match state.jump_to_spike() {
                Ok(()) => self.service_focus = false,
                Err(reason) => self.notice = Some(reason.to_string()),
            },
            KeyCode::Esc => state.cancel_spike(),
            KeyCode::Tab => {
                state.cancel_spike();
                self.service_focus = false;
            }
            KeyCode::Char('g') | KeyCode::End => {
                state.jump_live();
                self.service_focus = false;
            }
            _ => {}
        }
    }

    fn edit_search(&mut self, key: KeyEvent, state: &mut State) {
        match key.code {
            KeyCode::Esc => self.search = self.previous_search.take().unwrap_or_default(),
            KeyCode::Enter => self.previous_search = None,
            KeyCode::Backspace => {
                self.search.pop();
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.search.clear();
            }
            KeyCode::Char(character)
                if !character.is_control()
                    && !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                    && self.search.len() + character.len_utf8() <= MAX_SEARCH_BYTES =>
            {
                self.search.push(character);
            }
            _ => return,
        }
        state.set_search(&self.search);
    }
}

fn run(args: Args) -> io::Result<ExitCode> {
    if !io::stdout().is_terminal() {
        return Err(io::Error::other(
            "jscope needs a terminal on standard output",
        ));
    }
    if args.input.as_deref() == Some(Path::new("-")) && io::stdin().is_terminal() {
        return Err(io::Error::other(
            "--input - needs journal JSON piped or redirected to standard input",
        ));
    }

    let stop = Arc::new(AtomicBool::new(false));
    for signal in [SIGINT, SIGTERM, SIGHUP] {
        signal_hook::flag::register(signal, Arc::clone(&stop))?;
    }

    let mut state = args.initial_state()?;

    let _screen = Screen::enter()?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    let source = match args.input.as_deref() {
        Some(path) => Source::input(path).map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("opening {}: {error}", sanitize(&path.to_string_lossy())),
            )
        })?,
        None => Source::live(&JournalFilters {
            backlog: args.backlog,
            units: args.unit,
            user_units: args.user_unit,
            boot: args.boot,
            priority: args.priority,
        })
        .map_err(|error| io::Error::new(error.kind(), format!("starting journalctl: {error}")))?,
    };

    let started = Instant::now();
    let mut last_draw: Option<Instant> = None;
    let mut dirty = true;
    let mut controls = Controls::default();
    let mut input = InputStatus::new(args.input.is_some());

    'run: while !stop.load(Ordering::Relaxed) {
        // Sleep on intake so a burst wakes the consumer immediately. Bound the work
        // per turn so keyboard input and screen updates still get time under load.
        let first = (!input.ended).then(|| source.recv_timeout(POLL_INTERVAL));
        let now = started.elapsed().as_millis() as u64;
        state.advance(now);
        match first {
            Some(Ok(message)) => {
                input.accept(message, &mut state, now);
                for _ in 1..FRAME_BATCH {
                    let Ok(next) = source.try_recv() else { break };
                    input.accept(next, &mut state, now);
                }
                dirty = true;
            }
            Some(Err(RecvTimeoutError::Disconnected)) => {
                input.accept(
                    SourceMessage::Error("Input ended without a source status".into()),
                    &mut state,
                    now,
                );
                dirty = true;
            }
            _ => {}
        }

        let redraw_after = if dirty { POLL_INTERVAL } else { IDLE_REDRAW };
        if last_draw.is_none_or(|last| last.elapsed() >= redraw_after) {
            let status = ui::Status {
                source: input.label(),
                ended: input.ended,
                notice: controls.notice.as_deref().or(input.notice.as_deref()),
                malformed: input.malformed,
                oversized: input.oversized,
                service_focus: controls.service_focus,
                search_input: controls.search_input(),
            };
            let mut limit = 0;
            terminal.draw(|frame| {
                limit = ui::render(frame, &state, &status, args.no_color);
            })?;
            controls.detail_scroll_limit = limit;
            last_draw = Some(Instant::now());
            dirty = false;
        }

        for index in 0..32 {
            let timeout = if index == 0 && input.ended {
                POLL_INTERVAL
            } else {
                Duration::ZERO
            };
            match event::poll(timeout) {
                Ok(false) => break,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => break,
                result => result?,
            };
            dirty = true;
            match event::read() {
                Ok(TerminalEvent::Key(key)) if !controls.key(key, &mut state) => break 'run,
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::Interrupted => break,
                Err(error) => return Err(error),
            }
        }
    }

    if input.failed {
        Ok(ExitCode::FAILURE)
    } else {
        Ok(ExitCode::SUCCESS)
    }
}

fn main() -> ExitCode {
    match run(Args::parse()) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("jscope: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Args;
    use clap::Parser;
    use jscope::journal::{Decoder, Frame, MAX_LINE_BYTES};

    #[test]
    fn live_and_replay_start_with_every_received_priority_visible() {
        for argv in [&["jscope"][..], &["jscope", "--input", "-"]] {
            let args = Args::try_parse_from(argv).unwrap();
            let mut state = args.initial_state().unwrap();
            let demo = include_bytes!("../fixtures/demo.jsonl");
            let mut decoder = Decoder::new(demo.as_slice(), MAX_LINE_BYTES);
            while let Some(Frame::Event(event)) = decoder.next_frame().unwrap() {
                state.ingest(event, 0);
            }
            assert_eq!(state.counters().received, 11);
            assert_eq!(state.visible_rows().map(|row| row.count).sum::<u64>(), 11);
            state.toggle_mute();
            assert_eq!(state.visible_rows().map(|row| row.count).sum::<u64>(), 3);
            state.toggle_mute();
            assert_eq!(state.visible_rows().map(|row| row.count).sum::<u64>(), 11);
        }
    }

    #[test]
    fn replay_accepts_repeated_display_mutes() {
        let args = Args::try_parse_from([
            "jscope",
            "--input",
            "-",
            "--mute-unit",
            "worker.service",
            "--mute-unit",
            r"queue@disk\x2d1.service",
        ])
        .unwrap();
        let state = args.initial_state().unwrap();
        assert!(!state.is_muted());
        assert!(state.is_unit_muted("worker.service"));
        assert!(state.is_unit_muted(r"queue@disk\\x2d1.service"));
    }

    #[test]
    fn parses_journal_sources_and_native_filters() {
        let live = Args::try_parse_from([
            "jscope",
            "--backlog",
            "0",
            "--unit",
            "foo.service",
            "-u",
            "bar.*",
            "--user-unit",
            "mail.service",
            "--boot",
            "-1",
            "--priority",
            "warning..debug",
        ])
        .unwrap();
        assert_eq!(live.backlog, 0);
        assert_eq!(live.unit, ["foo.service", "bar.*"]);
        assert_eq!(live.user_unit, ["mail.service"]);
        assert_eq!(live.boot.as_deref(), Some("-1"));
        assert_eq!(live.priority.as_deref(), Some("warning..debug"));
        assert_eq!(
            live.initial_state().unwrap().grouping(),
            super::Grouping::Burst
        );

        let current =
            Args::try_parse_from(["jscope", "--boot", "--no-color", "--adjacent-only"]).unwrap();
        assert_eq!(current.boot.as_deref(), Some("0"));
        assert!(current.no_color);
        assert_eq!(
            current.initial_state().unwrap().grouping(),
            super::Grouping::Adjacent
        );

        let stdin = Args::try_parse_from(["jscope", "--input", "-"]).unwrap();
        assert_eq!(stdin.input.unwrap().to_str(), Some("-"));
        assert!(Args::try_parse_from(["jscope", "--input", "-", "--backlog", "12"]).is_err());
        assert!(Args::try_parse_from(["jscope", "--input", "-", "--unit", "a.service"]).is_err());
    }
}
