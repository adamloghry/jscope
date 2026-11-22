mod mouse;
pub use mouse::{MouseMap, MouseTarget};

use crate::journal::Event;
use crate::state::{
    BinSeverity, Group, Grouping, MessageOccurrence, SPARK_BINS, SpikeBin, SpikeSnapshot, State,
    Surroundings, UnitActivity, WINDOW_MS,
};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use std::borrow::Cow;
use unicode_width::UnicodeWidthStr;

#[derive(Clone, Copy, Debug, Default)]
pub struct Status<'a> {
    pub source: &'a str,
    pub ended: bool,
    pub notice: Option<&'a str>,
    pub malformed: u64,
    pub oversized: u64,
    pub service_focus: bool,
    pub search_input: Option<&'a str>,
}

pub fn render(frame: &mut Frame<'_>, state: &State, status: &Status<'_>, no_color: bool) -> usize {
    render_interactive(frame, state, status, no_color, &mut MouseMap::default())
}

pub fn render_interactive(
    frame: &mut Frame<'_>,
    state: &State,
    status: &Status<'_>,
    no_color: bool,
    mouse: &mut MouseMap,
) -> usize {
    mouse.clear();
    let area = frame.area();
    let wide = area.width >= 104;
    let context = state.context();
    let expanded = state.expanded_group().filter(|_| {
        context.is_none() && state.spike().is_none() && (wide || !status.service_focus)
    });
    let detail_height = expanded.map_or(0, |_| (area.height / 2).clamp(4, 12));
    let summaries = state.sidebar_activity();
    let diagnostics = diagnostic_line(state, status);
    let width = usize::from(area.width);
    let mut notices = Vec::new();
    wrap_line(&diagnostics, width, &mut notices);
    if let Some(message) = status.notice {
        let max_lines = notices.len() + 2;
        wrap_line(message, width, &mut notices);
        notices.truncate(max_lines);
    }
    let [header, alerts, leader, body, details, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(notices.len() as u16),
        Constraint::Length(u16::from(
            context.is_none() && !wide && !status.service_focus && !summaries.is_empty(),
        )),
        Constraint::Min(1),
        Constraint::Length(detail_height),
        Constraint::Length(1),
    ])
    .areas(area);

    render_header(frame, header, state, status, &summaries, no_color, mouse);
    frame.render_widget(
        Paragraph::new(notices.into_iter().map(Line::raw).collect::<Vec<_>>())
            .style(colored(Color::LightYellow, no_color)),
        alerts,
    );
    if !leader.is_empty()
        && let Some(entry) = summaries.first()
    {
        mouse.add(leader, MouseTarget::Leader);
        render_leader(frame, leader, state, entry, no_color);
    }
    render_footer(frame, footer, state, status, no_color, mouse);
    if let Some(context) = context {
        render_context(frame, body, context, no_color, mouse);
        return 0;
    }
    if let Some(occurrence) = state.spike().and_then(SpikeSnapshot::occurrence) {
        mouse.add(body, MouseTarget::Occurrence);
        return render_occurrence(frame, body, occurrence, state.detail_scroll(), no_color);
    }
    if wide {
        let [tail, activity] = Layout::horizontal([
            Constraint::Min(1),
            Constraint::Length((area.width / 3).min(52)),
        ])
        .areas(body);
        render_tail(frame, tail, state, status, no_color, mouse);
        render_activity(frame, activity, state, status, &summaries, no_color, mouse);
    } else if status.service_focus {
        render_activity(frame, body, state, status, &summaries, no_color, mouse);
    } else {
        render_tail(frame, body, state, status, no_color, mouse);
    }
    expanded.map_or(0, |group| {
        mouse.add(details, MouseTarget::Details);
        render_details(
            frame,
            details,
            group,
            state.spike_hit(),
            state.detail_scroll(),
            no_color,
        )
    })
}

pub fn render_closure_diff(
    frame: &mut Frame<'_>,
    state: &State,
    status: &Status<'_>,
    lines: &[String],
    scroll: usize,
    no_color: bool,
) -> usize {
    render_closure_diff_interactive(
        frame,
        state,
        status,
        lines,
        scroll,
        no_color,
        &mut MouseMap::default(),
    )
}

pub fn render_closure_diff_interactive(
    frame: &mut Frame<'_>,
    state: &State,
    status: &Status<'_>,
    lines: &[String],
    scroll: usize,
    no_color: bool,
    mouse: &mut MouseMap,
) -> usize {
    mouse.clear();
    let [header, body, keys] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .areas(frame.area());
    render_header(
        frame,
        header,
        state,
        status,
        &state.sidebar_activity(),
        no_color,
        mouse,
    );
    mouse.add(body, MouseTarget::Closure);
    render_keys(
        frame,
        keys,
        no_color,
        mouse,
        [
            ("↑↓", "scroll"),
            ("PgUp/Dn", "page"),
            ("Esc", "back"),
            ("g", "live"),
            ("q", "quit"),
        ],
    );
    render_scrollable_fields(
        frame,
        body,
        section("NixOS · closure changes".into(), true, no_color),
        lines,
        scroll,
    )
}

fn visible_line(area: Rect, index: usize) -> Rect {
    if index < usize::from(area.height) {
        Rect::new(area.x, area.y + index as u16, area.width, 1)
    } else {
        Rect::ZERO
    }
}

fn render_header(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &State,
    status: &Status<'_>,
    activity: &[UnitActivity],
    no_color: bool,
    mouse: &mut MouseMap,
) {
    let (mode, accent) = if state.is_paused() {
        ("Ⅱ Paused", Color::LightYellow)
    } else if status.ended {
        ("◆", Color::LightBlue)
    } else {
        ("● Live", Color::LightGreen)
    };
    let title = Line::from(vec![
        Span::styled(
            if area.width < 48 {
                "jscope"
            } else {
                "JournalScope"
            },
            colored(Color::LightCyan, no_color).add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!("  {mode}"), colored(accent, no_color)),
        Span::raw(format!(" {}", status.source)),
    ]);
    let title_width = title.width() as u16;
    let [identity, metrics] =
        Layout::horizontal([Constraint::Length(title_width), Constraint::Min(0)]).areas(area);
    mouse.add(identity, MouseTarget::Live);
    frame.render_widget(title, identity);
    let mut bins = [0; SPARK_BINS];
    let mut severity = [BinSeverity::Normal; SPARK_BINS];
    let mut incoming = 0;
    let mut visible = 0;
    for entry in activity {
        incoming += entry.incoming;
        visible += entry.visible;
        for (i, total) in bins.iter_mut().enumerate() {
            *total += entry.sparkline[i];
            severity[i] = severity[i].max(entry.severity[i]);
        }
    }
    let width = usize::from(metrics.width.saturating_sub(1));
    let mut spans = Vec::new();
    if incoming == 0 {
        spans.push(Span::styled(
            "Quiet · last 60s",
            Style::default().add_modifier(Modifier::DIM),
        ));
    } else {
        let rate = format!("{incoming}/min");
        let total = u128::from(incoming);
        let percent = ((u128::from(visible) * 100 + total / 2) / total) as usize;
        let share = format!(" {percent}% shown");
        let meter_width = width.saturating_sub(rate.len() + share.len() + 4).min(8);
        let graph_width = width
            .saturating_sub(rate.len() + share.len() + meter_width + 4)
            .min(SPARK_BINS);
        if width >= rate.len() + share.len() + meter_width + graph_width + 10 {
            spans.push(Span::styled(
                "Intake ",
                Style::default().add_modifier(Modifier::DIM),
            ));
        }
        if graph_width >= 3 {
            spans.extend(sparkline(
                &bins,
                &severity,
                graph_width,
                None,
                colored(Color::LightCyan, no_color),
                no_color,
            ));
            spans.push(Span::raw(" "));
        }
        spans.push(Span::styled(
            rate,
            Style::default().add_modifier(Modifier::BOLD),
        ));
        if meter_width >= 3 {
            let filled = (percent * meter_width + 50) / 100;
            spans.extend([
                Span::raw("  "),
                Span::styled("▰".repeat(filled), colored(Color::LightGreen, no_color)),
                Span::styled(
                    "▱".repeat(meter_width - filled),
                    colored(Color::DarkGray, no_color),
                ),
            ]);
        }
        if width >= spans.iter().map(Span::width).sum::<usize>() + share.len() {
            spans.push(Span::raw(share));
        }
    }
    frame.render_widget(Line::from(spans).right_aligned(), metrics);
}

fn diagnostic_line(state: &State, status: &Status<'_>) -> String {
    let counts = state.counters();
    let mut issues = Vec::new();
    if status.malformed > 0 {
        issues.push(format!("{} malformed", status.malformed));
    }
    if status.oversized > 0 {
        issues.push(format!("{} oversized", status.oversized));
    }
    if counts.evicted_rows > 0 {
        issues.push(format!(
            "evicted {} rows/{} events",
            counts.evicted_rows, counts.evicted_occurrences
        ));
    }
    if counts.activity_dropped > 0 {
        issues.push(format!("{} omitted from activity", counts.activity_dropped));
    }
    if issues.is_empty() {
        String::new()
    } else {
        format!("! {}", issues.join(" · "))
    }
}

fn filter_status(state: &State) -> String {
    let mut flags = String::new();
    if state.is_muted() {
        flags.push_str(" · warning+");
    }
    if let Some(unit) = state.solo() {
        flags.push_str(" · solo ");
        flags.push_str(unit);
    }
    if !state.search().is_empty() {
        flags.push_str(" · / ");
        flags.push_str(state.search());
    }
    flags
}

fn section(title: String, focused: bool, no_color: bool) -> Block<'static> {
    let accent = if focused {
        Color::LightCyan
    } else {
        Color::DarkGray
    };
    Block::default()
        .borders(Borders::TOP)
        .border_style(colored(accent, no_color))
        .title(Span::styled(
            format!(" {title} "),
            colored(
                if focused {
                    Color::LightCyan
                } else {
                    Color::Cyan
                },
                no_color,
            )
            .add_modifier(Modifier::BOLD),
        ))
}

fn render_tail(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &State,
    status: &Status<'_>,
    no_color: bool,
    mouse: &mut MouseMap,
) {
    mouse.add(area, MouseTarget::Tail);
    let grouping = match state.grouping() {
        Grouping::Burst => "bursts",
        Grouping::Adjacent => "adjacent",
    };
    let marker = state
        .spike_hit()
        .is_some_and(|hit| hit.generation().is_some());
    let landing = if marker && state.spike_hit_hidden() {
        " · Generation · outside filters"
    } else if marker {
        " · Generation"
    } else if state.spike_hit_hidden() {
        " · Spike · outside filters"
    } else if state.spike_hit().is_some() {
        " · Spike"
    } else {
        ""
    };
    let block = section(
        format!(
            "Journal{landing} · local · {grouping}{}",
            filter_status(state)
        ),
        !status.service_focus,
        no_color,
    );
    let content = block.inner(area);
    let mut lines = Vec::new();
    if let Some(selected) = state.selected_group() {
        let rows: Vec<_> = state
            .indexed_visible_rows()
            .rev()
            .skip_while(|(_, row)| row.id > selected.id)
            .take(usize::from(content.height))
            .collect();
        let mut rows = rows.into_iter().rev().peekable();
        let mut joins_previous = false;
        while let Some((index, row)) = rows.next() {
            let joins_next = state.grouping() == Grouping::Burst
                && rows.peek().is_some_and(|(next_index, next)| {
                    *next_index == index + 1 && next.continues_from(row)
                });
            let gutter = match (joins_previous, joins_next) {
                (false, true) => "╭ ",
                (true, true) => "│ ",
                (true, false) => "╰ ",
                (false, false) => "  ",
            };
            mouse.add(visible_line(content, lines.len()), MouseTarget::Row(row.id));
            lines.push(tail_line(
                row,
                if row.id == selected.id {
                    state.spike_hit()
                } else {
                    None
                },
                gutter,
                row.id == selected.id && state.is_paused() && !status.service_focus,
                state.show_hosts(),
                no_color,
                usize::from(content.width),
            ));
            joins_previous = joins_next;
        }
    } else {
        let empty = if state.is_paused() && state.visible_rows().next().is_some() {
            "Paused before the first entry · g to jump to live"
        } else if state.rows().is_empty() && status.ended {
            "Input ended without journal entries"
        } else if state.rows().is_empty() {
            "Waiting for journal entries…"
        } else if !state.search().is_empty() {
            "No entries match this search · / to edit"
        } else if state.solo().is_some() {
            "No visible entries for the soloed service · s to clear solo"
        } else if state.is_muted() {
            "Lower priorities are muted · m to show them"
        } else {
            "Service entries are muted · Tab to select a service, M to unmute"
        };
        lines.push(Line::from(empty));
    }

    frame.render_widget(block, area);
    frame.render_widget(Paragraph::new(lines), content);
}

fn tail_line<'a>(
    row: &'a Group,
    hit: Option<&'a Event>,
    gutter: &'static str,
    selected: bool,
    show_host: bool,
    no_color: bool,
    width: usize,
) -> Line<'a> {
    let event = hit.unwrap_or(&row.first);
    let (severity, severity_style) = severity(event.priority, no_color);
    let first = crate::local_time::format(event.realtime_us);
    let stamp = if width < 65 && hit.is_none() {
        first[..8].to_owned()
    } else {
        first
    };
    let repeat = if row.count > 1 {
        format!(
            " ×{}/{} ",
            row.count,
            elapsed(row.first.realtime_us, row.last.realtime_us)
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
    let short_unit = if event.generation().is_some() {
        "◆ NixOS"
    } else {
        event
            .unit
            .strip_suffix(".service")
            .filter(|stem| !stem.is_empty())
            .unwrap_or(&event.unit)
    };
    let unit = truncate_display(short_unit, available.saturating_sub(12).min(available / 2));
    let message = truncate_display(&event.message, available.saturating_sub(unit.width()));
    let unit_style = unit_style(&event.unit, no_color);
    let mut spans = vec![
        if selected {
            Span::raw("› ")
        } else {
            Span::styled(gutter, unit_style)
        },
        Span::styled(stamp, Style::default().add_modifier(Modifier::DIM)),
        Span::raw(repeat),
    ];
    if show_host {
        spans.push(Span::raw(host));
        spans.push(Span::raw(" "));
    }
    spans.extend([
        Span::styled(unit, unit_style),
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

fn unit_tags(state: &State, unit: &str) -> String {
    let mut tags = String::new();
    if state.is_pinned(unit) {
        tags.push_str(" [pin]");
    }
    if state.solo() == Some(unit) {
        tags.push_str(" [solo]");
    }
    if state.is_unit_muted(unit) {
        tags.push_str(" [muted]");
    }
    tags
}

fn render_leader(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &State,
    entry: &UnitActivity,
    no_color: bool,
) {
    let label = "Noisiest ";
    let legend = if area.width >= 72 { " in/vis/supp" } else { "" };
    let width = usize::from(area.width).saturating_sub(label.width() + legend.len());
    let line = if entry.incoming == 0 {
        Line::from("Activity · quiet over the last 60s")
    } else {
        let mut spans = vec![Span::styled(label, colored(Color::LightCyan, no_color))];
        spans.extend(activity_spans(state, entry, width, None, no_color));
        spans.push(Span::styled(
            legend,
            Style::default().add_modifier(Modifier::DIM),
        ));
        Line::from(spans)
    };
    frame.render_widget(line, area);
}

fn render_activity(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &State,
    status: &Status<'_>,
    summaries: &[UnitActivity],
    no_color: bool,
    mouse: &mut MouseMap,
) {
    if let Some(spike) = state.spike() {
        render_spike(frame, area, spike, no_color, mouse);
        return;
    }
    mouse.add(area, MouseTarget::Services);
    let title = if status.service_focus {
        format!("Services · 60s{}", filter_status(state))
    } else {
        "Services · 60s".to_owned()
    };
    let block = section(title, status.service_focus, no_color);
    let content = block.inner(area);
    frame.render_widget(block, area);
    let [legend, list] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(content);
    let dim = Style::default().add_modifier(Modifier::DIM);
    let recovered = summaries
        .iter()
        .any(|unit| state.recovery(&unit.unit).is_some());
    let width = usize::from(legend.width);
    let mut key = Line::from(vec![
        Span::styled("↑", rising_style(no_color)),
        Span::styled(" rising ", dim),
        spark_bar("▆", BinSeverity::Warning, dim, no_color),
        Span::styled(" warn ", dim),
        spark_bar("▆", BinSeverity::Error, dim, no_color),
        Span::styled(" err+", dim),
    ]);
    if recovered {
        let marker = Span::styled("↓ recovered", colored(Color::LightGreen, no_color));
        if key.width() + marker.width() + "  in/vis/supp".len() <= width {
            key.push_span(Span::raw(" "));
            key.push_span(marker);
        } else {
            key = Line::from(marker);
        }
    }
    let counts = if key.width() + 22 <= usize::from(legend.width) {
        "in/visible/suppressed"
    } else {
        "in/vis/supp"
    };
    if key.width() + counts.len() < usize::from(legend.width) {
        frame.render_widget(key, legend);
    }
    frame.render_widget(Line::styled(counts, dim).right_aligned(), legend);
    let width = usize::from(list.width.saturating_sub(2));
    let bars = trend_width(width);
    let peak = summaries
        .iter()
        .flat_map(|entry| buckets(&entry.sparkline, bars).map(|bin| bin.iter().sum()))
        .max();
    let selected_index = state.sidebar_index(summaries);
    let capacity = usize::from(list.height);
    let start = if status.service_focus {
        (selected_index + 1).saturating_sub(capacity)
    } else {
        0
    };
    let mut lines = Vec::new();
    for (index, entry) in summaries.iter().enumerate().skip(start).take(capacity) {
        mouse.add(
            visible_line(list, lines.len()),
            MouseTarget::Service(entry.unit.clone()),
        );
        let selected = index == selected_index && status.service_focus;
        let mut spans = vec![Span::raw(if selected { "› " } else { "  " })];
        spans.extend(activity_spans(state, entry, width, peak, no_color));
        let line = Line::from(spans);
        lines.push(if selected {
            line.style(Style::default().add_modifier(Modifier::REVERSED))
        } else {
            line
        });
    }
    if summaries.is_empty() {
        lines.push(Line::from("No intake in the last 60s"));
    }
    frame.render_widget(Paragraph::new(lines), list);
}

fn render_spike(
    frame: &mut Frame<'_>,
    area: Rect,
    spike: &SpikeSnapshot,
    no_color: bool,
    mouse: &mut MouseMap,
) {
    mouse.add(area, MouseTarget::Spike);
    let block = section("Spike · 60s snapshot".into(), true, no_color);
    let content = block.inner(area);
    frame.render_widget(block, area);
    let accent = colored(Color::LightCyan, no_color).add_modifier(Modifier::BOLD);
    let dim = Style::default().add_modifier(Modifier::DIM);
    let width = usize::from(content.width);
    let mut lanes = vec![(spike.unit.as_str(), &spike.bins, true)];
    if let Some(peer) = &spike.peer {
        lanes.push((peer.unit.as_str(), &peer.bins, false));
        if spike.peer_first {
            lanes.swap(0, 1);
        }
    }
    let paired = lanes.len() == 2;
    let peak = lanes
        .iter()
        .flat_map(|(_, bins, _)| bins.iter().map(|bin| bin.count))
        .max()
        .unwrap_or(0);
    let show_times = paired && width >= 28;
    let name_width = lanes
        .iter()
        .map(|(name, _, _)| name.width())
        .max()
        .unwrap_or(0)
        .min(width.saturating_sub(if show_times { 16 } else { 2 }));
    let mut lines = Vec::new();
    for (name, bins, active) in lanes {
        let style = unit_style(name, no_color);
        let bin = &bins[spike.selected];
        if paired {
            let name = truncate_display(name, name_width);
            let pad = name_width - name.width() + 2;
            let mut label = vec![
                Span::styled(if active { "› " } else { "  " }, accent),
                Span::styled(name, style),
            ];
            if show_times && let Some(event) = bin.event() {
                label.push(Span::raw(" ".repeat(pad)));
                label.push(Span::styled(
                    crate::local_time::format(event.realtime_us),
                    dim,
                ));
            }
            lines.push(Line::from(label));
        } else {
            lines.push(Line::styled(truncate_display(name, width), style));
        }
        let (mut chart, bin_widths) =
            spike_chart(bins, spike.selected, peak, style, no_color, paired);
        let chart_row = visible_line(content, lines.len());
        let mut column = 0;
        for (index, width) in bin_widths.into_iter().enumerate() {
            mouse.span(
                chart_row,
                column,
                width,
                MouseTarget::Bin {
                    peer: !active,
                    index,
                },
            );
            column += width;
        }
        if paired {
            let count = format!("  {} in", bin.count);
            let count = if chart.width() + count.width() <= width {
                count
            } else {
                format!("  {}", compact_count(bin.count))
            };
            chart.push_span(Span::styled(count, dim));
        }
        lines.push(chart);
    }
    if let Some(marker) = spike.recovery {
        lines.push(Line::styled(
            format!(
                "Recovered {}s ago · burst {}s",
                marker.age_ms / 1_000,
                marker.duration_ms / 1_000
            ),
            colored(Color::LightGreen, no_color),
        ));
    }
    let bin = &spike.bins[spike.selected];
    let seconds = WINDOW_MS / SPARK_BINS as u64 / 1_000;
    let age = (SPARK_BINS - 1 - spike.selected) as u64 * seconds;
    lines.push(Line::styled(
        format!("{age}–{}s before capture", age + seconds),
        dim,
    ));
    if let Some(event) = bin.event() {
        let target = match bin.severity {
            BinSeverity::Normal => "first arrival",
            BinSeverity::Warning => "first warning",
            BinSeverity::Error => "first error or higher",
        };
        let entries = if bin.count == 1 { "entry" } else { "entries" };
        lines.push(Line::from(format!("{} {entries} · {target}", bin.count)));
        let (label, severity_style) = severity(event.priority, no_color);
        lines.push(Line::from(vec![
            Span::styled(crate::local_time::format(event.realtime_us), dim),
            Span::raw(" "),
            Span::styled(label, severity_style),
        ]));
        lines.push(Line::raw(truncate_display(&event.message, width)));
        lines.push(Line::from(vec![
            Span::styled(
                if spike.patterns {
                    "Top patterns"
                } else {
                    "Top messages"
                },
                accent,
            ),
            Span::styled(" · prev→now/total", dim),
        ]));
        for (rank, driver) in bin.drivers.iter().enumerate() {
            mouse.add(visible_line(content, lines.len()), MouseTarget::Rank(rank));
            let previous = driver
                .previous_count
                .map_or_else(|| "?".into(), |n| n.to_string());
            let share = format!("{previous}→{}/{} · ", driver.count, bin.count);
            let key = format!("[{}] ", rank + 1);
            let message = truncate_display(
                &driver.message,
                width.saturating_sub(key.width() + share.width()),
            );
            lines.push(Line::from(vec![
                Span::styled(key, dim),
                Span::styled(share, accent),
                Span::raw(message),
            ]));
        }
    } else {
        lines.push(Line::from("0 entries · empty interval"));
    }
    frame.render_widget(Paragraph::new(lines), content);
}

fn spike_chart(
    bins: &[SpikeBin; SPARK_BINS],
    selected: usize,
    peak: u64,
    style: Style,
    no_color: bool,
    paired: bool,
) -> (Line<'static>, [usize; SPARK_BINS]) {
    let cursor = colored(Color::LightCyan, no_color).add_modifier(Modifier::BOLD);
    let mut chart = Vec::new();
    let mut widths = [0; SPARK_BINS];
    for (index, bin) in bins.iter().enumerate() {
        let start = chart.len();
        let height = spark_height(bin.count, peak);
        if index == selected {
            chart.push(Span::styled("[", cursor));
        }
        if paired && no_color {
            chart.push(spark_bar(" ", bin.severity, style, true));
            chart.push(Span::styled(height, style));
        } else {
            if index != selected && index != selected + 1 {
                chart.push(Span::styled(" ", cursor));
            }
            chart.push(spark_bar(height, bin.severity, style, no_color));
        }
        if index == selected {
            chart.push(Span::styled("]", cursor));
        }
        widths[index] = chart[start..].iter().map(Span::width).sum();
    }
    (Line::from(chart), widths)
}

fn activity_spans<'a>(
    state: &State,
    entry: &'a UnitActivity,
    width: usize,
    peak: Option<u64>,
    no_color: bool,
) -> Vec<Span<'a>> {
    let tags = unit_tags(state, &entry.unit);
    let bars = trend_width(width);
    let mut spans = Vec::new();
    let width = if entry.rising {
        spans.push(Span::styled("↑ ", rising_style(no_color)));
        width.saturating_sub(2)
    } else if let Some(marker) = state.recovery(&entry.unit) {
        let label = if width >= 48 { "↓ recovered " } else { "↓" };
        let marker = format!("{label}{}s ", marker.duration_ms / 1_000);
        let remaining = width.saturating_sub(marker.width());
        spans.push(Span::styled(marker, colored(Color::LightGreen, no_color)));
        remaining
    } else {
        width
    };
    let mut counts = format!("{}/{}/{}", entry.incoming, entry.visible, entry.suppressed);
    if counts.len() + 10 + tags.width() > width {
        counts = format!(
            "{}/{}/{}",
            compact_count(entry.incoming),
            compact_count(entry.visible),
            compact_count(entry.suppressed)
        );
    }
    let available = width.saturating_sub(tags.width() + counts.width() + 2);
    let bars = if available >= bars + 4 { bars } else { 0 };
    let unit_width = available.saturating_sub(bars);
    let unit = truncate_display(&entry.unit, unit_width);
    let padding = unit_width.saturating_sub(unit.width());
    let style = unit_style(&entry.unit, no_color);
    spans.extend([
        Span::styled(unit, style),
        Span::styled(tags, colored(Color::LightYellow, no_color)),
        Span::raw(" ".repeat(padding + 1)),
    ]);
    spans.extend(sparkline(
        &entry.sparkline,
        &entry.severity,
        bars,
        peak,
        style,
        no_color,
    ));
    spans.push(Span::raw(format!(" {counts}")));
    spans
}

fn render_details(
    frame: &mut Frame<'_>,
    area: Rect,
    group: &Group,
    hit: Option<&Event>,
    scroll: usize,
    no_color: bool,
) -> usize {
    let entry = hit.unwrap_or(&group.last);
    let host = entry.host.as_deref().unwrap_or("(unknown)");
    let cursor = group.last.cursor.as_deref().unwrap_or("(missing)");
    let priority = severity(entry.priority, true).0;
    let mut fields = vec![format!(
        "{} · host {host} · {priority} · {} {}",
        entry.unit,
        group.count,
        if group.count == 1 {
            "occurrence"
        } else {
            "occurrences"
        }
    )];
    if let Some(generation) = entry.generation() {
        fields.push(format!(
            "Previous system  {}",
            generation
                .previous
                .as_deref()
                .unwrap_or("(first activation this boot)")
        ));
        fields.push(format!("Current system   {}", generation.current));
    }
    let realtime = |timestamp: Option<u64>| {
        timestamp.map_or_else(|| "(missing)".to_owned(), |us| us.to_string())
    };
    if let Some(hit) = hit {
        let label = if hit.generation().is_some() {
            "Generation"
        } else {
            "Spike"
        };
        fields.push(format!(
            "{label} local {} · Journal realtime µs {}",
            crate::local_time::format(hit.realtime_us),
            realtime(hit.realtime_us)
        ));
        fields.push(format!(
            "{label} cursor {}",
            hit.cursor.as_deref().unwrap_or("(missing)")
        ));
    }
    if group.count > 1 {
        fields.push(format!(
            "Journal realtime µs {} → {}",
            realtime(group.first.realtime_us),
            realtime(group.last.realtime_us),
        ));
    } else if hit.is_none() {
        fields.push(format!(
            "Local {} · Journal realtime µs {}",
            crate::local_time::format(entry.realtime_us),
            realtime(entry.realtime_us)
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
            .map(|timestamp| crate::local_time::format(*timestamp))
            .collect::<Vec<_>>()
            .join("  ");
        let omitted = group.count - group.recent_times().len() as u64;
        let earlier = if omitted > 0 {
            format!("  ({omitted} earlier)")
        } else {
            String::new()
        };
        fields.push(format!("Recent local  {times}{earlier}"));
    }
    fields.push(format!("Message  {}", entry.message));

    let block = section("Details · PgUp/PgDn scroll".into(), false, no_color);
    render_scrollable_fields(frame, area, block, &fields, scroll)
}

fn render_context(
    frame: &mut Frame<'_>,
    area: Rect,
    context: &Surroundings,
    no_color: bool,
    mouse: &mut MouseMap,
) {
    mouse.add(area, MouseTarget::Context);
    let mut title = format!("Context ±2s · {} retained", context.events.len());
    if context.omitted > 0 {
        title.push_str(&format!(" · {} more", context.omitted));
    }
    let block = section(title, true, no_color);
    let content = block.inner(area);
    frame.render_widget(block, area);
    let [rows, info] = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(content);
    let height = usize::from(rows.height);
    let start = context
        .selected
        .saturating_sub(height / 2)
        .min(context.events.len().saturating_sub(height));
    let mut lines = Vec::new();
    for (i, event) in context.events.iter().enumerate().skip(start).take(height) {
        mouse.add(visible_line(rows, lines.len()), MouseTarget::ContextRow(i));
        let (label, style) = severity(event.priority, no_color);
        let width = usize::from(rows.width);
        let host = truncate_display(event.host.as_deref().unwrap_or("?"), (width / 6).min(20));
        let unit = truncate_display(
            event.unit.strip_suffix(".service").unwrap_or(&event.unit),
            width / 4,
        );
        let stamp = crate::local_time::format(event.realtime_us);
        let mut line = Line::from(vec![
            Span::raw(if i == context.selected { "›" } else { " " }),
            Span::styled(
                if i == context.anchor { "◆ " } else { "  " },
                colored(Color::LightCyan, no_color),
            ),
            Span::styled(stamp, Style::default().add_modifier(Modifier::DIM)),
            Span::raw(" "),
            Span::raw(host),
            Span::raw(" "),
            Span::styled(unit, unit_style(&event.unit, no_color)),
            Span::raw(" "),
            Span::styled(label, style),
            Span::raw(" "),
            Span::raw(event.message.as_str()),
        ]);
        if i == context.selected {
            line = line.style(Style::default().add_modifier(Modifier::BOLD));
        }
        lines.push(line);
    }
    frame.render_widget(Paragraph::new(lines), rows);
    let selected = &context.events[context.selected];
    frame.render_widget(
        Line::raw(format!(
            "{}/{} · Cursor {}",
            context.selected + 1,
            context.events.len(),
            selected.cursor.as_deref().unwrap_or("(missing)")
        )),
        info,
    );
}

fn render_occurrence(
    frame: &mut Frame<'_>,
    area: Rect,
    occurrence: MessageOccurrence<'_>,
    scroll: usize,
    no_color: bool,
) -> usize {
    let event = occurrence.event;
    let ordinal = format!(
        "Occurrence {}/{}",
        occurrence.index + 1,
        occurrence.retained
    );
    let omitted = occurrence.total - occurrence.retained as u64;
    let count = if omitted == 0 {
        ordinal
    } else {
        format!("{ordinal} retained · {omitted} repeat details omitted")
    };
    let fields = [
        count,
        format!(
            "{} · {} · host {}",
            event.unit,
            severity(event.priority, no_color).0,
            event.host.as_deref().unwrap_or("(unknown)")
        ),
        format!(
            "Local {} · Journal realtime µs {}",
            crate::local_time::format(event.realtime_us),
            event
                .realtime_us
                .map_or_else(|| "(missing)".into(), |us| us.to_string())
        ),
        format!("Cursor {}", event.cursor.as_deref().unwrap_or("(missing)")),
        event.message.clone(),
    ];
    let block = section("Message occurrences".into(), true, no_color);
    render_scrollable_fields(frame, area, block, &fields, scroll)
}

fn render_scrollable_fields(
    frame: &mut Frame<'_>,
    area: Rect,
    block: Block<'_>,
    fields: &[String],
    scroll: usize,
) -> usize {
    let content = block.inner(area);
    let mut lines = Vec::new();
    for field in fields {
        wrap_line(field, usize::from(content.width), &mut lines);
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

fn wrap_line<'a>(text: &'a str, width: usize, lines: &mut Vec<&'a str>) {
    if width == 0 || text.is_empty() {
        return;
    }
    // Details and source diagnostics have already had control characters escaped.
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

fn render_footer(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &State,
    status: &Status<'_>,
    no_color: bool,
    mouse: &mut MouseMap,
) {
    if let Some(query) = status.search_input {
        let hint = if area.width >= 40 {
            "  Enter apply · Esc cancel"
        } else {
            ""
        };
        let [input, keys] =
            Layout::horizontal([Constraint::Min(1), Constraint::Length(hint.width() as u16)])
                .areas(area);
        let query = format!("/ {query}▏");
        let offset = query.width().saturating_sub(usize::from(input.width)) as u16;
        frame.render_widget(Paragraph::new(query).scroll((0, offset)), input);
        if !hint.is_empty() {
            mouse.shortcut(keys, 2, "Enter", "Enter apply".width());
            mouse.shortcut(
                keys,
                "  Enter apply · ".width(),
                "Esc",
                "Esc cancel".width(),
            );
        }
        frame.render_widget(Line::raw(hint), keys);
        return;
    }
    let mut keys = Vec::new();
    if state.context().is_some() {
        keys.extend([
            ("↑↓", "browse"),
            ("◆", "anchor"),
            ("Esc", "back"),
            ("PgUp/Dn", "page"),
            ("g", "live"),
            ("q", "quit"),
        ]);
    } else if state.spike().and_then(SpikeSnapshot::occurrence).is_some() {
        keys.extend([
            ("←→", "occurrence"),
            ("c", "context"),
            ("Enter", "jump"),
            ("Esc", "back"),
            ("PgUp/Dn", "scroll"),
            ("g", "live"),
            ("q", "quit"),
        ]);
    } else if let Some(spike) = state.spike() {
        if spike.peer.is_some() {
            keys.push(("↑↓", "service"));
        }
        keys.extend([
            ("←→", "interval"),
            ("Enter", "jump"),
            ("1–3", "follow"),
            ("t", if spike.patterns { "exact" } else { "patterns" }),
            ("Esc", "back"),
            ("g", "live"),
            ("q", "quit"),
        ]);
    } else {
        keys.extend([
            ("q", "quit"),
            (
                "Tab",
                if status.service_focus {
                    "journal"
                } else {
                    "services"
                },
            ),
            ("↑↓", "move"),
        ]);
        if status.service_focus {
            keys.extend([("Enter", "spike"), ("p", "pin"), ("s", "solo")]);
        } else {
            if state
                .selected_group()
                .is_some_and(|row| row.first.generation().is_some())
            {
                keys.push(("d", "packages"));
            }
            keys.extend([("↵", "view"), ("[]", "spikes"), ("c", "context")]);
            if state
                .rows()
                .iter()
                .any(|row| row.first.generation().is_some())
            {
                keys.push(("G", "generation"));
            }
        }
        keys.extend([
            ("g", "live"),
            ("/", "find"),
            ("m/M", "mute"),
            ("Space", if state.is_paused() { "resume" } else { "pause" }),
        ]);
    }
    render_keys(frame, area, no_color, mouse, keys);
}

fn render_keys<'a>(
    frame: &mut Frame<'_>,
    area: Rect,
    no_color: bool,
    mouse: &mut MouseMap,
    keys: impl IntoIterator<Item = (&'a str, &'a str)>,
) {
    let mut spans = Vec::new();
    let separator = if area.width >= 88 { "  " } else { " " };
    let mut remaining = usize::from(area.width);
    for (key, action) in keys {
        let width = key.width() + 1 + action.width();
        if width > remaining {
            break;
        }
        mouse.shortcut(area, usize::from(area.width) - remaining, key, width);
        spans.push(Span::styled(
            key,
            colored(Color::LightCyan, no_color).add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::raw(format!(" {action}{separator}")));
        remaining = remaining.saturating_sub(width + separator.len());
    }
    frame.render_widget(Line::from(spans), area);
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

fn trend_width(width: usize) -> usize {
    if width >= 48 {
        SPARK_BINS
    } else {
        SPARK_BINS / 2
    }
}

fn buckets<T>(bins: &[T; SPARK_BINS], bars: usize) -> impl Iterator<Item = &[T]> + Clone {
    (0..bars).map(move |i| &bins[i * SPARK_BINS / bars..(i + 1) * SPARK_BINS / bars])
}

fn rising_style(no_color: bool) -> Style {
    colored(Color::LightGreen, no_color).add_modifier(Modifier::BOLD)
}

fn spark_bar(
    height: &'static str,
    severity: BinSeverity,
    normal: Style,
    no_color: bool,
) -> Span<'static> {
    let (symbol, color) = match severity {
        BinSeverity::Normal => return Span::styled(height, normal),
        BinSeverity::Warning => ("!", Color::LightYellow),
        BinSeverity::Error => ("×", Color::LightRed),
    };
    Span::styled(
        if no_color { symbol } else { height },
        colored(color, no_color).add_modifier(Modifier::BOLD),
    )
}

fn sparkline<'a>(
    bins: &'a [u64; SPARK_BINS],
    severity: &'a [BinSeverity; SPARK_BINS],
    bars: usize,
    peak: Option<u64>,
    normal: Style,
    no_color: bool,
) -> impl Iterator<Item = Span<'a>> + 'a {
    let counts = buckets(bins, bars).map(|bin| bin.iter().sum::<u64>());
    let max = peak.unwrap_or_else(|| counts.clone().max().unwrap_or(0));
    counts
        .zip(buckets(severity, bars))
        .map(move |(count, severities)| {
            spark_bar(
                spark_height(count, max),
                severities.iter().copied().max().unwrap_or_default(),
                normal,
                no_color,
            )
        })
}

fn spark_height(count: u64, peak: u64) -> &'static str {
    const LEVELS: [&str; 8] = ["▁", "▂", "▃", "▄", "▅", "▆", "▇", "█"];
    if count == 0 {
        return "·";
    }
    LEVELS[(u128::from(count) * 7 / u128::from(peak)) as usize]
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
    use ratatui::buffer::{Buffer, Cell};

    fn event(message: &str, priority: u8, realtime_us: u64) -> Event {
        Event {
            realtime_us: Some(realtime_us),
            cursor: Some("fixture-cursor".to_string()),
            priority: Some(priority),
            host: Some("fixture-host".to_string()),
            unit: "noisy.service".to_string(),
            unit_metadata: UnitMetadata::new(UnitField::SystemUnit, Box::new([])),
            generation: None,
            message: message.to_string(),
        }
    }

    fn render_buffer(
        width: u16,
        height: u16,
        state: &State,
        status: &Status<'_>,
        no_color: bool,
    ) -> Buffer {
        render_mapped(
            width,
            height,
            state,
            status,
            no_color,
            &mut MouseMap::default(),
        )
    }

    fn render_mapped(
        width: u16,
        height: u16,
        state: &State,
        status: &Status<'_>,
        no_color: bool,
        map: &mut MouseMap,
    ) -> Buffer {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| {
                render_interactive(frame, state, status, no_color, map);
            })
            .unwrap();
        terminal.backend().buffer().clone()
    }

    fn lines(buffer: &Buffer) -> Vec<String> {
        buffer
            .content()
            .chunks(usize::from(buffer.area.width))
            .map(|row| row.iter().map(|cell| cell.symbol()).collect())
            .collect()
    }

    fn screen(width: u16, height: u16, state: &State, status: &Status<'_>) -> Vec<String> {
        lines(&render_buffer(width, height, state, status, true))
    }

    fn spike_replay() -> State {
        let mut state = State::default();
        for (at, priority, message, cursor) in [
            (0, 4, "queue slow", "warning"),
            (7_000, 3, "queue failed", "first-in-group"),
            (16_000, 3, "queue failed", "first-in-bin"),
            (16_100, 3, "queue failed", "last-in-group"),
        ] {
            let mut entry = event(message, priority, 1_740_000_000_000_000 + at * 1_000);
            entry.cursor = Some(cursor.into());
            state.ingest(entry, at);
        }
        state.set_search("excluded");
        assert!(state.begin_spike());
        state
    }

    #[test]
    fn spike_inspector_keeps_all_twelve_intervals_and_their_first_matches() {
        let mut state = spike_replay();
        let status = Status {
            service_focus: true,
            ..Default::default()
        };
        for width in [80, 104] {
            for no_color in [true, false] {
                let buf = render_buffer(width, 24, &state, &status, no_color);
                let rows = lines(&buf);
                let view = rows.join("\n");
                assert!(view.contains("Spike · 60s snapshot"), "{view}");
                assert!(view.contains("noisy.service"), "{view}");
                assert!(view.contains("0–5s before capture"), "{view}");
                assert!(view.contains("2 entries · first error or higher"), "{view}");
                assert!(view.contains("prev→now/total"), "{view}");
                assert!(view.contains("1→2/2 · queue failed"), "{view}");
                assert!(
                    view.contains("Enter jump") && view.contains("Esc back"),
                    "{view}"
                );
                let (y, chart) = rows
                    .iter()
                    .enumerate()
                    .find(|(_, line)| line.contains('['))
                    .unwrap();
                let bins = chart.chars().filter(|c| "·!×▁▂▃▄▅▆▇█".contains(*c)).count();
                assert_eq!(bins, SPARK_BINS, "{chart}");
                if no_color {
                    assert!(chart.contains('!') && chart.contains("[×]"), "{chart}");
                } else {
                    let (left, _) = chart.split_once("[█]").unwrap();
                    assert_eq!(buf[(left.width() as u16 + 1, y as u16)].fg, Color::LightRed);
                }
            }
        }
        state.advance(90_000);
        state.move_spike(-1);
        let view = screen(80, 24, &state, &status).join("\n");
        assert!(
            view.contains("5–10s before capture") && view.contains("1 entry · first error"),
            "{view}"
        );
        state.move_spike(-1);
        let view = screen(80, 24, &state, &status).join("\n");
        assert!(
            view.contains("[·]") && view.contains("0 entries · empty interval"),
            "{view}"
        );
    }

    #[test]
    fn paired_charts_keep_one_scale_cursor_and_lane_order_when_switching() {
        let mut state = State::default();
        let target_us = 1_740_000_000_000_000;
        let long_warning = "queue lag ".repeat(100);
        state.ingest(event(&long_warning, 4, target_us), 40_000);
        for message in ["retry", "retry", "retry", "retry", "retry", "wait", "wait"] {
            state.ingest(event(message, 6, target_us), 40_000);
        }
        let mut peer = event("socket failed", 3, target_us + 3_000_000);
        peer.unit = "db.service".into();
        state.ingest(peer, 40_000);
        state.set_search("hide all rows");
        let status = Status {
            service_focus: true,
            ..Default::default()
        };
        state.toggle_pin().unwrap();
        let services = screen(80, 24, &state, &status).join("\n");
        assert!(
            services.contains("noisy.service [pin]") && services.contains("p pin"),
            "{services}"
        );
        state.move_unit(1);
        state.toggle_pin().unwrap();
        state.move_unit(-1);
        assert!(state.begin_spike());
        for width in [80, 104] {
            for no_color in [true, false] {
                let mut map = MouseMap::default();
                let buffer = render_mapped(width, 24, &state, &status, no_color, &mut map);
                let rows = lines(&buffer);
                let view = rows.join("\n");
                assert!(
                    view.contains("Top messages") && view.contains("↑↓ service"),
                    "{view}"
                );
                assert!(
                    view.contains("5/8 · retry") && view.contains("2/8 · wait"),
                    "{view}"
                );
                assert!(
                    view.contains("1/8 · queue lag") && view.contains('…'),
                    "{view}"
                );
                let a = rows
                    .iter()
                    .position(|line| line.contains("› noisy.service"))
                    .unwrap();
                let b = rows
                    .iter()
                    .position(|line| line.contains("  db.service"))
                    .unwrap();
                let time_a = crate::local_time::format(Some(target_us));
                let time_b = crate::local_time::format(Some(target_us + 3_000_000));
                let clock_x = |line: &str, stamp: &str| line.split_once(stamp).unwrap().0.width();
                assert_eq!(clock_x(&rows[a], &time_a), clock_x(&rows[b], &time_b));
                assert!(a < b);
                let (bar_a, bar_b) = if no_color {
                    ("[!█]", "[×▁]")
                } else {
                    ("[█]", "[▁]")
                };
                let x = rows[a + 1].split_once(bar_a).unwrap().0.width() as u16;
                assert_eq!(x, rows[b + 1].split_once(bar_b).unwrap().0.width() as u16);
                assert!(rows[a + 1].contains("8 in") && rows[b + 1].contains("1 in"));
                for (peer, row, selected_bar) in [(false, a + 1, bar_a), (true, b + 1, bar_b)] {
                    let target = MouseTarget::Bin {
                        peer,
                        index: SPARK_BINS - 1,
                    };
                    for col in x..x + selected_bar.width() as u16 {
                        assert_eq!(map.target(col, row as u16), Some(&target));
                    }
                    assert_eq!(
                        map.target(x + selected_bar.width() as u16, row as u16),
                        Some(&MouseTarget::Spike)
                    );
                }
                if no_color {
                    assert!(buffer.content().iter().all(|cell| cell.fg == Color::Reset));
                } else {
                    assert_eq!(buffer[(x + 1, a as u16 + 1)].fg, Color::LightYellow);
                    assert_eq!(buffer[(x + 1, b as u16 + 1)].fg, Color::LightRed);
                }
                state.switch_spike_unit();
                let swapped = lines(&render_buffer(width, 24, &state, &status, no_color));
                assert!(
                    swapped[a].contains("  noisy.service") && swapped[b].contains("› db.service")
                );
                assert_eq!(
                    (&rows[a + 1], &rows[b + 1]),
                    (&swapped[a + 1], &swapped[b + 1])
                );
                assert!(
                    swapped
                        .iter()
                        .any(|line| line.contains("1/1 · socket failed"))
                );
                state.switch_spike_unit();
            }
        }
    }

    #[test]
    fn hit_regions_follow_clipped_rows_diagnostics_and_resizes() {
        use crossterm::event::KeyCode;
        let mut state = State::default();
        for i in 0..22 {
            state.ingest(event(&format!("receipt-{i:02}"), 6, i), i);
        }
        let mut map = MouseMap::default();
        let status = Status {
            notice: Some("notice"),
            ..Default::default()
        };
        let rows = lines(&render_mapped(104, 12, &state, &status, false, &mut map));
        let mut found = 0;
        for (row_index, row) in rows.iter().enumerate() {
            if let Some(start) = row.find("receipt-") {
                let message = &row[start..start + "receipt-00".len()];
                let id = state
                    .rows()
                    .iter()
                    .find(|g| g.first.message == message)
                    .unwrap()
                    .id;
                assert_eq!(
                    map.target(row[..start].width() as u16, row_index as u16),
                    Some(&MouseTarget::Row(id))
                );
                found += 1;
            }
        }
        assert!(found > 0 && found < state.rows().len());
        assert_eq!(map.target(1, 1), None); // diagnostic
        let small = lines(&render_mapped(22, 6, &state, &status, false, &mut map));
        assert_eq!(map.target(103, 5), None); // discarded wide frame
        assert_eq!(map.target(0, 2), Some(&MouseTarget::Leader));
        assert_eq!(
            map.target(0, 5),
            Some(&MouseTarget::Shortcut(KeyCode::Char('q')))
        );
        assert_eq!(map.target(6, 5), None); // separator after "q quit"
        assert!(small.last().unwrap().starts_with("q quit Tab services"));
        assert_eq!(map.target(21, 5), None); // first clipped shortcut
    }

    #[test]
    fn jump_lands_on_filtered_repeat_with_its_own_timestamp_and_cursor() {
        let mut state = spike_replay();
        state.jump_to_spike().unwrap();
        let status = Status::default();
        let view = screen(80, 24, &state, &status);
        assert!(
            view.iter()
                .any(|line| line.contains("Spike · outside filters"))
        );
        let hit = view.iter().find(|line| line.starts_with("› ")).unwrap();
        let time = crate::local_time::format(Some(1_740_000_016_000_000));
        assert!(
            hit.contains(&time) && hit.contains("×3/9s") && hit.contains("ERROR queue failed"),
            "{hit}"
        );
        state.toggle_expanded();
        let view = screen(80, 24, &state, &status).join("\n");
        assert!(view.contains("Spike cursor first-in-bin"), "{view}");
        assert!(view.contains("Latest cursor last-in-group"), "{view}");
        assert!(
            view.contains("Journal realtime µs 1740000016000000"),
            "{view}"
        );
        assert!(
            view.contains("1740000007000000 → 1740000016100000"),
            "{view}"
        );
    }

    #[test]
    fn log_labels_shorten_only_the_trailing_service_suffix() {
        let status = Status::default();
        let mut state = State::default();
        for (name, source) in [
            ("worker@east.service", UnitField::SystemUnit),
            ("notify.service.service", UnitField::UserUnit),
            ("bus.service.socket", UnitField::SystemUnit),
            ("sync.timer", UnitField::SystemUnit),
            ("agent.service", UnitField::SyslogIdentifier),
            (".service", UnitField::Command),
        ] {
            let mut entry = event("target.service ready", 5, 0);
            entry.unit = name.to_owned();
            entry.unit_metadata = UnitMetadata::new(source, Box::new([]));
            state.ingest(entry, 0);
        }
        let display = screen(80, 24, &state, &status).join("\n");
        for expected in [
            "worker@east",
            "notify.service",
            "bus.service.socket",
            "sync.timer",
            "agent",
            ".service",
        ] {
            assert!(
                display.contains(&format!(" {expected} NOTICE target.service ready")),
                "{display}"
            );
        }
        state.set_search("worker@east.service");
        let display = screen(80, 24, &state, &status).join("\n");
        assert!(
            display.contains(" worker@east NOTICE target.service ready"),
            "{display}"
        );
        assert!(!display.contains(" agent NOTICE"), "{display}");
        state.toggle_expanded();
        let display = screen(80, 24, &state, &status).join("\n");
        assert!(
            display.contains("_SYSTEMD_UNIT=worker@east.service"),
            "{display}"
        );
        let services = Status {
            service_focus: true,
            ..Default::default()
        };
        let list = screen(80, 24, &state, &services);
        assert!(
            list.iter()
                .any(|line| line.contains("worker@east.service") && line.contains("1/1/0")),
            "{list:?}"
        );
    }

    #[test]
    fn burst_gutter_marks_routine_runs_and_stops_at_hidden_boundaries() {
        let mut state = State::default();
        for (i, message) in [
            "task one",
            "task two",
            "task three",
            "interruption",
            "task four",
        ]
        .into_iter()
        .enumerate()
        {
            let priority = if message == "interruption" { 4 } else { 6 };
            state.ingest(event(message, priority, i as u64), i as u64);
        }
        let status = Status::default();
        let gutters = |state: &State| {
            screen(80, 24, state, &status)
                .into_iter()
                .filter(|line| line.contains("noisy INFO task"))
                .map(|row| row.chars().next().unwrap())
                .collect::<String>()
        };
        assert_eq!(gutters(&state), "╭│╰ ");
        state.set_search("task");
        assert_eq!(gutters(&state), "╭│╰ ");
        state.move_row(-1);
        assert_eq!(gutters(&state), "╭│›");
        state.jump_live();
        state.set_grouping(Grouping::Adjacent);
        assert_eq!(gutters(&state), "    ");
    }

    #[test]
    fn identical_short_labels_keep_separate_unit_colors_and_gutters() {
        let mut state = State::default();
        let mut process = event("process report", 6, 0);
        process.unit = "noisy".to_owned();
        process.unit_metadata = UnitMetadata::new(UnitField::Command, Box::new([]));
        state.ingest(process, 0);
        state.ingest(event("service report", 6, 1), 1);
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|frame| {
                render(frame, &state, &Status::default(), false);
            })
            .unwrap();
        let rows = terminal.backend().buffer().content().chunks(80);
        let mut colors = Vec::new();
        for cells in rows {
            let text: String = cells.iter().map(|cell| cell.symbol()).collect();
            if let Some(label) = text.find("noisy INFO ") {
                assert!(text.starts_with("  "), "{text}");
                colors.push(cells[text[..label].width()].fg);
            }
        }
        assert_eq!(
            colors,
            [
                unit_style("noisy", false).fg.unwrap(),
                unit_style("noisy.service", false).fg.unwrap()
            ]
        );
    }

    #[test]
    fn header_tracks_the_rolling_window_and_sidebar_trends_share_a_scale() {
        let mut state = State::default();
        for _ in 0..8 {
            state.ingest(event("burst", 6, 0), 0);
        }
        let mut other = event("alarm", 4, 0);
        other.unit = "other.service".into();
        state.ingest(other, 55_000);
        state.toggle_mute();
        let status = Status {
            source: "journalctl",
            ..Default::default()
        };
        let view = lines(&render_buffer(144, 24, &state, &status, false));
        assert!(view[0].contains("█··········▁ 9/min"), "{}", view[0]);
        assert!(view[0].contains("▰▱▱▱▱▱▱▱ 11% shown"), "{}", view[0]);
        // The older burst and the single fresh event should have different heights.
        assert!(
            view.iter()
                .any(|row| row.contains('█') && row.contains("8/0/8"))
        );
        assert!(
            view.iter()
                .any(|row| row.contains('▁') && row.contains("1/1/0"))
        );
        state.advance(60_000);
        let view = screen(80, 24, &state, &status);
        assert!(view[0].contains("1/min") && view[0].contains("100% shown"));
        state.advance(115_000);
        let view = screen(80, 24, &state, &status);
        assert!(view[0].contains("Quiet"));
        assert!(view.iter().any(|row| row.contains("WARN alarm")));
    }

    #[test]
    fn rising_and_severity_survive_filtering_and_chart_compression() {
        let mut state = State::default();
        for (at, priority) in [(15_000, 6), (20_000, 4), (25_000, 4)] {
            state.ingest(event("activity", priority, at * 1_000), at);
        }
        let mut quiet = event("other error", 3, 25_000_000);
        quiet.unit = "quiet.service".into();
        state.ingest(quiet, 25_000);
        for priority in [3, 6, 7] {
            state.ingest(event("activity", priority, 30_000_000), 30_000);
        }
        state.set_search("hide everything");

        fn chart_before<'a>(row: &'a [Cell], suffix: &str, bars: usize) -> &'a [Cell] {
            let text: String = row.iter().map(|cell| cell.symbol()).collect();
            let end = text[..text.find(suffix).unwrap()].width();
            &row[end - bars..end]
        }

        for (width, service_row, bars, label) in [
            (80, 1, 12, "Noisiest ↑ noisy.service"),
            (104, 3, 6, "› ↑ noisy.service"),
        ] {
            let status = Status {
                service_focus: width == 104,
                ..Default::default()
            };
            for no_color in [false, true] {
                let buffer = render_buffer(width, 24, &state, &status, no_color);
                let display = lines(&buffer);
                assert!(
                    display[service_row].contains(label),
                    "{}",
                    display[service_row]
                );
                assert!(!display.iter().any(|line| line.contains("↑ quiet.service")));
                let mut rows = buffer.content().chunks(usize::from(width));
                let header_chart = chart_before(rows.next().unwrap(), " 7/min", SPARK_BINS);
                // Only quiet.service logged an error in this five-second header bin.
                let from_other = &header_chart[10];
                let chart = chart_before(rows.nth(service_row - 1).unwrap(), " 6/0/6", bars);
                // At six columns, the final pair of bins contains both a warning and an error.
                let warning = &chart[bars - 2];
                let error = &chart[bars - 1];
                if no_color {
                    assert_eq!(
                        (warning.symbol(), error.symbol(), from_other.symbol()),
                        ("!", "×", "×")
                    );
                    assert!(buffer.content().iter().all(|cell| cell.fg == Color::Reset));
                } else {
                    assert_eq!(warning.fg, Color::LightYellow);
                    assert_eq!(error.fg, Color::LightRed);
                    assert_eq!(from_other.fg, Color::LightRed);
                    assert!(warning.modifier.contains(Modifier::BOLD));
                    assert!(error.modifier.contains(Modifier::BOLD));
                    assert_eq!(warning.symbol(), if bars == 12 { "▃" } else { "▄" });
                    assert_eq!(error.symbol(), "█");
                }
            }
        }
        state.advance(35_000);
        let view = screen(80, 24, &state, &Status::default());
        assert!(view[1].starts_with("Noisiest noisy.service"), "{}", view[1]);
    }

    #[test]
    fn confirmed_recovery_keeps_the_unit_and_counts_on_one_line() {
        let mut state = State::default();
        for _ in 0..3 {
            state.ingest(event("burst", 6, 123), 16_000);
        }
        state.set_muted(true);
        state.advance(27_000);
        for no_color in [true, false] {
            for (width, label) in [
                (80, "↓ recovered 5s noisy.service"),
                (104, "↓5s noisy.service"),
            ] {
                let buffer = render_buffer(width, 24, &state, &Status::default(), no_color);
                let view = lines(&buffer);
                let (row, line) = view
                    .iter()
                    .enumerate()
                    .find(|(_, line)| line.contains(label) && line.contains("3/0/3"))
                    .expect("recovery and counts share the service row");
                let col = line[..line.find(label).unwrap()].width();
                assert_eq!(
                    buffer[(col as u16, row as u16)].fg,
                    if no_color {
                        Color::Reset
                    } else {
                        Color::LightGreen
                    }
                );
            }
        }
    }

    #[test]
    fn service_inspector_freezes_recovery_time_for_the_inspected_lane() {
        let mut state = State::default();
        for _ in 0..3 {
            state.ingest(event("alarm", 4, 0), 16_000);
        }
        let mut other = event("one message", 6, 1);
        other.unit = "peer.service".into();
        state.ingest(other, 18_000);
        state.advance(30_000);
        state.toggle_pin().unwrap();
        state.move_unit(1);
        state.begin_spike();
        let status = Status {
            service_focus: true,
            ..Default::default()
        };
        assert!(
            !screen(80, 24, &state, &status)
                .join("\n")
                .contains("Recovered ")
        );
        state.switch_spike_unit();
        state.advance(100_000);
        for width in [80, 104] {
            let view = screen(width, 24, &state, &status).join("\n");
            assert!(view.contains("Recovered 8s ago · burst 5s"), "{view}");
        }
    }

    #[test]
    fn explains_empty_sources_frozen_tails_and_filtered_rows() {
        let mut state = State::default();
        let mut status = Status::default();
        let display = |state: &State, status: &Status<'_>| screen(80, 12, state, status).join("\n");
        assert!(display(&state, &status).contains("Waiting for journal entries"));
        status.ended = true;
        assert!(display(&state, &status).contains("Input ended without journal entries"));
        state.set_paused(true);
        state.ingest(event("background task", 6, 0), 0);
        assert!(display(&state, &status).contains("g to jump to live"));
        state.jump_live();
        state.toggle_mute();
        assert!(display(&state, &status).contains("m to show them"));
        state.set_search("unseen");
        assert!(display(&state, &status).contains("/ to edit"));
    }

    #[test]
    fn muted_noise_stays_in_sidebar_and_warning_stays_in_tail() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = State::default();
        state.set_muted(true);
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
        assert!(display.contains("! 3600/1200/2400"), "{display}");
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
