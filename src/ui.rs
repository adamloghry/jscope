use crate::state::{Group, SPARK_BINS, State, UnitActivity};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use std::borrow::Cow;
use unicode_width::UnicodeWidthStr;

#[derive(Clone, Copy, Debug, Default)]
pub struct Status<'a> {
    pub source: &'a str,
    pub notice: Option<&'a str>,
    pub malformed: u64,
    pub oversized: u64,
    pub service_focus: bool,
    pub search_input: Option<&'a str>,
}

pub fn render(frame: &mut Frame<'_>, state: &State, status: &Status<'_>, no_color: bool) -> usize {
    let area = frame.area();
    let expanded = state.expanded_group();
    let detail_height = expanded.map_or(0, |_| (area.height / 2).clamp(4, 12));
    let counters = counter_line(state, status);
    let counter_height = if counters.width() > usize::from(area.width) {
        2
    } else {
        1
    };
    let notice_height = if status.notice.is_some() { 2 } else { 0 };
    let [header, body, details, footer] = Layout::vertical([
        Constraint::Length(1 + counter_height + notice_height),
        Constraint::Min(4),
        Constraint::Length(detail_height),
        Constraint::Length(if area.width < 80 { 3 } else { 2 }),
    ])
    .areas(area);

    render_header(frame, header, state, status, counters, counter_height);
    let [tail, activity] = if body.width >= 96 {
        Layout::horizontal([Constraint::Min(30), Constraint::Length(30)]).areas(body)
    } else {
        let activity_height = (body.height / 3).max(4).min(body.height.saturating_sub(3));
        Layout::vertical([Constraint::Min(3), Constraint::Length(activity_height)]).areas(body)
    };
    render_tail(frame, tail, state, status.service_focus, no_color);
    render_activity(frame, activity, state, status.service_focus, no_color);
    render_footer(frame, footer, state, status);
    expanded.map_or(0, |group| {
        render_details(frame, details, group, state.detail_scroll())
    })
}

fn render_header(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &State,
    status: &Status<'_>,
    counter_line: String,
    counter_height: u16,
) {
    let scrolling = if state.is_paused() { "Paused" } else { "Live" };
    let [title, counters, notice] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(counter_height),
        Constraint::Min(0),
    ])
    .areas(area);
    let title_line = Line::from(vec![
        Span::styled(
            "JournalScope",
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!("  {scrolling} · {}", status.source)),
    ]);
    frame.render_widget(Paragraph::new(title_line), title);
    frame.render_widget(
        Paragraph::new(counter_line).wrap(Wrap { trim: true }),
        counters,
    );
    if let Some(text) = status.notice {
        frame.render_widget(Paragraph::new(text).wrap(Wrap { trim: true }), notice);
    }
}

fn counter_line(state: &State, status: &Status<'_>) -> String {
    let counts = state.counters();
    format!(
        "In {} · bad JSON {} · oversize {} · evicted {} rows/{} events · activity omitted {}",
        counts.received,
        status.malformed,
        status.oversized,
        counts.evicted_rows,
        counts.evicted_occurrences,
        counts.activity_dropped,
    )
}

fn render_tail(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &State,
    service_focus: bool,
    no_color: bool,
) {
    let block = panel("Journal · UTC", !service_focus);
    let content = block.inner(area);
    let mut lines = Vec::new();
    if let Some(selected) = state.selected_group() {
        lines.extend(
            state
                .visible_rows()
                .rev()
                .skip_while(|row| row.id > selected.id)
                .take(usize::from(content.height))
                .map(|row| {
                    tail_line(
                        row,
                        row.id == selected.id && state.is_paused() && !service_focus,
                        state.show_hosts(),
                        content.width < 65,
                        no_color,
                        usize::from(content.width),
                    )
                }),
        );
        lines.reverse();
    } else {
        lines.push(Line::from("No matching entries"));
    }

    frame.render_widget(block, area);
    frame.render_widget(Paragraph::new(lines), content);
}

fn tail_line(
    row: &Group,
    selected: bool,
    show_host: bool,
    compact: bool,
    no_color: bool,
    width: usize,
) -> Line<'_> {
    let event = &row.first;
    let (severity, severity_style) = severity(event.priority, no_color);
    let first = time(event.realtime_us);
    let stamp = if compact {
        first[..8].to_owned()
    } else {
        first
    };
    let repeat = if row.count > 1 {
        format!(
            " ×{}/{} ",
            row.count,
            elapsed(event.realtime_us, row.last.realtime_us)
        )
    } else {
        " ".to_owned()
    };
    let prefix_width = 2 + stamp.width() + repeat.width();
    let available =
        width.saturating_sub(prefix_width + severity.width() + 2 + usize::from(show_host));
    let host = if show_host {
        truncate_display(
            event.host.as_deref().unwrap_or("?"),
            available.saturating_sub(22).min(12),
        )
    } else {
        Cow::Borrowed("")
    };
    let available = available.saturating_sub(host.width());
    let unit = truncate_display(&event.unit, available.saturating_sub(12).min(available / 2));
    let message = truncate_display(&event.message, available.saturating_sub(unit.width()));
    let mut spans = vec![
        Span::raw(if selected { "› " } else { "  " }),
        Span::raw(stamp),
        Span::raw(repeat),
    ];
    if show_host {
        spans.push(Span::raw(host));
        spans.push(Span::raw(" "));
    }
    spans.extend([
        Span::styled(unit, unit_style(&event.unit, no_color)),
        Span::raw(" "),
        Span::styled(severity, severity_style),
        Span::raw(" "),
        Span::raw(message),
    ]);
    let line = Line::from(spans);
    if selected {
        line.style(Style::default().add_modifier(Modifier::BOLD))
    } else {
        line
    }
}

fn truncate_display(text: &str, width: usize) -> Cow<'_, str> {
    if width == 0 {
        return Cow::Borrowed("");
    }
    if text.len() <= width {
        return Cow::Borrowed(text);
    }
    if text.is_ascii() {
        return Cow::Owned(format!("{}…", &text[..width - 1]));
    }
    if text.width() <= width {
        return Cow::Borrowed(text);
    }
    let mut result = String::new();
    let mut used = 0;
    for grapheme in Span::raw(text).styled_graphemes(Style::default()) {
        let cells = grapheme.symbol.width();
        if used + cells >= width {
            break;
        }
        result.push_str(grapheme.symbol);
        used += cells;
    }
    result.push('…');
    Cow::Owned(result)
}

fn render_activity(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &State,
    service_focus: bool,
    no_color: bool,
) {
    let block = panel("Activity · 60s", service_focus);
    let content = block.inner(area);
    let summaries = state.sidebar_activity();
    let selected_index = state.sidebar_index(&summaries);
    let width = usize::from(content.width);
    let capacity = usize::from(content.height / 2);
    let start = (selected_index + 1).saturating_sub(capacity);
    let mut lines = Vec::new();

    for (index, entry) in summaries.iter().enumerate().skip(start).take(capacity) {
        let selected = index == selected_index;
        let marker = if selected && service_focus {
            "› "
        } else {
            "  "
        };
        let solo = if state.solo() == Some(entry.unit.as_str()) {
            " [solo]"
        } else {
            ""
        };
        let muted = if state.is_unit_muted(&entry.unit) {
            " [muted]"
        } else {
            ""
        };
        let unit_width = width.saturating_sub(marker.width() + solo.len() + muted.len());
        let label = vec![
            Span::raw(marker),
            Span::styled(
                truncate_display(&entry.unit, unit_width),
                unit_style(&entry.unit, no_color),
            ),
            Span::raw(solo),
            Span::raw(muted),
        ];
        lines.push(Line::from(label));
        lines.push(Line::from(activity_counts(entry, width)));
    }

    if summaries.is_empty() {
        lines.push(Line::from("Waiting for intake"));
    }

    frame.render_widget(block, area);
    frame.render_widget(Paragraph::new(lines), content);
}

fn render_details(frame: &mut Frame<'_>, area: Rect, group: &Group, scroll: usize) -> usize {
    let entry = &group.last;
    let host = entry.host.as_deref().unwrap_or("(unknown)");
    let cursor = entry.cursor.as_deref().unwrap_or("(missing)");
    let priority = severity(entry.priority, true).0;
    let mut fields = vec![format!(
        "{} · host {host} · {priority} · {} occurrences",
        entry.unit, group.count
    )];
    let realtime = |timestamp: Option<u64>| {
        timestamp.map_or_else(|| "(missing)".to_owned(), |us| us.to_string())
    };
    if group.count == 1 {
        fields.push(format!(
            "Journal realtime µs {}",
            realtime(entry.realtime_us)
        ));
    } else {
        fields.push(format!(
            "Journal realtime µs {} → {}",
            realtime(group.first.realtime_us),
            realtime(entry.realtime_us),
        ));
    }
    fields.extend(
        entry
            .unit_fields()
            .map(|(field, value)| format!("{}={value}", field.journal_name())),
    );
    fields.push(format!("Latest cursor {cursor}"));
    if group.count > 1 {
        let times = group
            .recent_times()
            .iter()
            .map(|timestamp| time(*timestamp))
            .collect::<Vec<_>>()
            .join("  ");
        let omitted = group.count - group.recent_times().len() as u64;
        let earlier = if omitted > 0 {
            format!("  ({omitted} earlier)")
        } else {
            String::new()
        };
        fields.push(format!("Recent UTC  {times}{earlier}"));
    }
    fields.push(format!("Message  {}", entry.message));

    let block = panel("Details · PgUp/PgDn scroll", false);
    let content = block.inner(area);
    let mut lines = Vec::new();
    for field in &fields {
        wrap_detail_line(field, usize::from(content.width), &mut lines);
    }
    let height = usize::from(content.height);
    let limit = lines.len().saturating_sub(height);
    let visible = lines
        .into_iter()
        .skip(scroll.min(limit))
        .take(height)
        .map(Line::raw)
        .collect::<Vec<_>>();
    frame.render_widget(block, area);
    frame.render_widget(Paragraph::new(visible), content);
    limit
}

fn wrap_detail_line<'a>(text: &'a str, width: usize, lines: &mut Vec<&'a str>) {
    if width == 0 {
        return;
    }
    // Details are composed from already escaped fields and contain no newlines.
    if text.is_ascii() {
        lines.extend(
            (0..text.len())
                .step_by(width)
                .map(|start| &text[start..(start + width).min(text.len())]),
        );
        return;
    }
    let mut start = 0;
    let mut end = 0;
    let mut used = 0;
    for grapheme in Span::raw(text).styled_graphemes(Style::default()) {
        let cells = grapheme.symbol.width();
        if end > start && used + cells > width {
            lines.push(&text[start..end]);
            start = end;
            used = 0;
        }
        used += cells;
        end += grapheme.symbol.len();
    }
    lines.push(&text[start..]);
}

fn render_footer(frame: &mut Frame<'_>, area: Rect, state: &State, status: &Status<'_>) {
    let mute = if state.is_muted() {
        "warning+"
    } else {
        "all priorities"
    };
    let query = match status.search_input {
        Some(editing) => format!("/ {editing}▏  Enter apply  Esc cancel"),
        None => {
            let search = if state.search().is_empty() {
                String::new()
            } else {
                format!(" · / {}", state.search())
            };
            let solo = state
                .solo()
                .map_or(String::new(), |unit| format!(" · solo {unit}"));
            format!("Activity in/visible/suppressed · {mute}{solo}{search}")
        }
    };
    let keys = "q quit  Tab pane  ↑↓ move  ↵ view  s solo  m/M mute  / find  Space pause  g live";
    let [filters, shortcuts] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(area);
    let offset = if status.search_input.is_some() {
        query.width().saturating_sub(usize::from(filters.width)) as u16
    } else {
        0
    };
    frame.render_widget(Paragraph::new(query).scroll((0, offset)), filters);
    frame.render_widget(Paragraph::new(keys).wrap(Wrap { trim: true }), shortcuts);
}

fn panel(title: &'static str, focused: bool) -> Block<'static> {
    let style = if focused {
        Style::default().add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    };
    Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(title, style))
}

fn time(realtime_us: Option<u64>) -> String {
    let Some(us) = realtime_us else {
        return "--:--:--.---".to_owned();
    };
    let ms = us / 1_000;
    let hour = ms / 3_600_000 % 24;
    let min = ms / 60_000 % 60;
    let sec = ms / 1_000 % 60;
    let fraction = ms % 1_000;
    format!("{hour:02}:{min:02}:{sec:02}.{fraction:03}")
}

fn elapsed(first: Option<u64>, last: Option<u64>) -> String {
    let (Some(first), Some(last)) = (first, last) else {
        return "?".to_owned();
    };
    let sign = if last < first { "−" } else { "" };
    let span = last.abs_diff(first);
    if span < 1_000 {
        format!("{sign}{span}µs")
    } else if span < 1_000_000 {
        format!("{sign}{}ms", span / 1_000)
    } else {
        format!("{sign}{}s", span / 1_000_000)
    }
}

fn activity_counts(entry: &UnitActivity, width: usize) -> String {
    let mut counts = format!("{}/{}/{}", entry.incoming, entry.visible, entry.suppressed);
    if counts.len() + 8 > width {
        counts = format!(
            "{}/{}/{}",
            compact_count(entry.incoming),
            compact_count(entry.visible),
            compact_count(entry.suppressed),
        );
    }
    let bars = width.saturating_sub(counts.len() + 4).min(SPARK_BINS);
    if bars == 0 {
        return counts;
    }
    format!("  {}  {counts}", sparkline(entry, bars))
}

fn compact_count(mut count: u64) -> String {
    let mut scale = "";
    for suffix in ["k", "M", "G", "T", "P", "E"] {
        if count < 1_000 {
            break;
        }
        count /= 1_000;
        scale = suffix;
    }
    format!("{count}{scale}")
}

fn sparkline(entry: &UnitActivity, bars: usize) -> String {
    const LEVELS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    let counts = (0..bars)
        .map(|index| {
            entry.sparkline[index * SPARK_BINS / bars..(index + 1) * SPARK_BINS / bars]
                .iter()
                .sum::<u64>()
        })
        .collect::<Vec<_>>();
    let max = counts.iter().copied().max().unwrap_or(0);
    counts
        .into_iter()
        .map(|count| {
            if count == 0 {
                '·'
            } else {
                let bar = (count as u128 * 7 / max as u128) as usize;
                LEVELS[bar]
            }
        })
        .collect()
}

fn severity(priority: Option<u8>, no_color: bool) -> (&'static str, Style) {
    let (name, color) = match priority {
        Some(0) => ("EMERG", Color::LightRed),
        Some(1) => ("ALERT", Color::LightRed),
        Some(2) => ("CRIT", Color::LightRed),
        Some(3) => ("ERROR", Color::LightRed),
        Some(4) => ("WARN", Color::LightYellow),
        Some(5) => ("NOTICE", Color::LightBlue),
        Some(6) => ("INFO", Color::White),
        Some(7) => ("DEBUG", Color::Gray),
        _ => ("UNKNOWN", Color::White),
    };
    (name, colored(color, no_color))
}

fn unit_style(unit: &str, no_color: bool) -> Style {
    const COLORS: [Color; 8] = [
        Color::Cyan,
        Color::Green,
        Color::LightBlue,
        Color::LightMagenta,
        Color::LightGreen,
        Color::Yellow,
        Color::LightCyan,
        Color::Magenta,
    ];
    let hash = unit.as_bytes().iter().fold(2_166_136_261u32, |hash, byte| {
        (hash ^ u32::from(*byte)).wrapping_mul(16_777_619)
    });
    colored(COLORS[hash as usize % COLORS.len()], no_color)
}

fn colored(color: Color, no_color: bool) -> Style {
    if no_color {
        Style::default()
    } else {
        Style::default().fg(color)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::{Event, UnitField, UnitMetadata};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn event(message: &str, priority: u8, realtime_us: u64) -> Event {
        Event {
            realtime_us: Some(realtime_us),
            cursor: Some("fixture-cursor".to_string()),
            priority: Some(priority),
            host: Some("fixture-host".to_string()),
            unit: "noisy.service".to_string(),
            unit_metadata: UnitMetadata::new(UnitField::SystemUnit, Box::new([])),
            message: message.to_string(),
        }
    }

    #[test]
    fn muted_noise_stays_in_sidebar_and_warning_stays_in_tail() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = State::default();
        for timestamp in 0..5 {
            state.ingest(event("quietly repeated", 6, timestamp), 0);
        }
        state.ingest(event("visible warning", 4, 6000), 1);
        state.ingest(event("visible warning", 4, 16000), 2);
        terminal
            .draw(|frame| {
                render(frame, &state, &Status::default(), true);
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let display: String = buffer.content().iter().map(|cell| cell.symbol()).collect();
        assert!(display.contains("7/2/5"));
        assert!(display.contains("WARN visible warning"));
        assert!(display.contains("×2/10ms"));
        assert!(!display.contains("quietly repeated"));
        assert!(!display.contains("fixture-host"));
        assert!(buffer.content().iter().all(|cell| cell.fg == Color::Reset));

        state.toggle_expanded();
        terminal
            .draw(|frame| {
                render(frame, &state, &Status::default(), true);
            })
            .unwrap();
        let display: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(display.contains("fixture-host"));
        assert!(display.contains("fixture-cursor"));
    }

    #[test]
    fn detail_scroll_reaches_full_message_and_keeps_latest_metadata() {
        let mut state = State::default();
        let message = format!("{} message-end", "log content ".repeat(140));
        let mut first = event(&message, 4, 1_740_000_000_000_001);
        first.unit_metadata = UnitMetadata::new(UnitField::SyslogIdentifier, Box::new([]));
        first.cursor = Some("first-cursor".to_string());
        let mut last = event(&message, 4, 1_740_000_000_000_007);
        last.unit_metadata = UnitMetadata::new(
            UnitField::UserUnit,
            Box::new([(UnitField::SystemUnit, "session.scope".into())]),
        );
        last.cursor = Some(format!("{}latest-cursor", "a".repeat(260)));
        state.ingest(first, 0);
        state.ingest(last, 1);
        state.toggle_expanded();

        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        let status = Status::default();
        let mut limit = 0;
        terminal
            .draw(|frame| {
                limit = render(frame, &state, &status, true);
            })
            .unwrap();
        let display = |terminal: &Terminal<TestBackend>| -> String {
            terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .map(|cell| cell.symbol())
                .collect()
        };
        let initial = display(&terminal);
        assert!(
            initial.contains("_SYSTEMD_USER_UNIT=noisy.service"),
            "{initial}"
        );
        assert!(initial.contains("_SYSTEMD_UNIT=session.scope"), "{initial}");
        assert!(
            initial.contains("1740000000000001 → 1740000000000007"),
            "{initial}"
        );
        assert!(initial.contains("latest-cursor"), "{initial}");
        assert!(!initial.contains("first-cursor"));
        assert!(!initial.contains("message-end"));

        assert!(limit > 0);
        state.scroll_details(isize::MAX, limit);
        terminal
            .draw(|frame| {
                render(frame, &state, &status, true);
            })
            .unwrap();
        let end = display(&terminal);
        assert!(end.contains("message-end"), "{end}");
    }

    #[test]
    fn sidebar_preserves_burst_counts_and_service_mute_on_long_units() {
        let mut state = State::default();
        state.set_muted(false);
        let mut warning = event("warning", 4, 1);
        warning.unit = "中文-".repeat(12);
        let mut info = warning.clone();
        info.priority = Some(6);
        for _ in 0..1_200 {
            state.ingest(warning.clone(), 0);
        }
        for _ in 0..2_400 {
            state.ingest(info.clone(), 0);
        }
        state.set_unit_muted(&info.unit, true).unwrap();
        let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
        terminal
            .draw(|frame| {
                render(frame, &state, &Status::default(), true);
            })
            .unwrap();
        let display: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(display.contains("3600/1200/2400"), "{display}");
        assert!(display.contains("[muted]"), "{display}");
        assert!(display.contains("█"), "{display}");
    }

    #[test]
    fn truncation_preserves_extended_grapheme_clusters() {
        assert_eq!(truncate_display("👩‍💻XY", 3), "👩‍💻…");
        assert_eq!(truncate_display("ABCDE", 3), "AB…");
    }

    #[test]
    fn long_metadata_keeps_severity_and_message_in_the_tail() {
        let mut state = State::default();
        let unit = format!("{}service", "long-".repeat(20));
        let mut first = event("first message", 4, 1);
        first.unit = unit.clone();
        first.host = Some("host-one-with-a-long-name".to_owned());
        state.ingest(first, 0);
        let mut second = event("warning text remains visible", 4, 2);
        second.unit = unit;
        second.host = Some("host-two-with-a-long-name".to_owned());
        state.ingest(second, 1);

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                render(frame, &state, &Status::default(), true);
            })
            .unwrap();
        let display: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(display.contains("WARN warning text"), "{display}");
        assert!(display.contains("host-two-wi…"), "{display}");
    }
}
