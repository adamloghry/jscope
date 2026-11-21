use crate::state::{BinSeverity, Group, Grouping, SPARK_BINS, State, UnitActivity};
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
    let area = frame.area();
    let wide = area.width >= 104;
    let expanded = state
        .expanded_group()
        .filter(|_| wide || !status.service_focus);
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
            !wide && !status.service_focus && !summaries.is_empty(),
        )),
        Constraint::Min(1),
        Constraint::Length(detail_height),
        Constraint::Length(1),
    ])
    .areas(area);

    render_header(frame, header, state, status, &summaries, no_color);
    frame.render_widget(
        Paragraph::new(notices.into_iter().map(Line::raw).collect::<Vec<_>>())
            .style(colored(Color::LightYellow, no_color)),
        alerts,
    );
    if !leader.is_empty()
        && let Some(entry) = summaries.first()
    {
        render_leader(frame, leader, state, entry, no_color);
    }
    if wide {
        let [tail, activity] = Layout::horizontal([
            Constraint::Min(1),
            Constraint::Length((area.width / 3).min(52)),
        ])
        .areas(body);
        render_tail(frame, tail, state, status, no_color);
        render_activity(frame, activity, state, status, &summaries, no_color);
    } else if status.service_focus {
        render_activity(frame, body, state, status, &summaries, no_color);
    } else {
        render_tail(frame, body, state, status, no_color);
    }
    render_footer(frame, footer, status, no_color);
    expanded.map_or(0, |group| {
        render_details(frame, details, group, state.detail_scroll(), no_color)
    })
}

fn render_header(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &State,
    status: &Status<'_>,
    activity: &[UnitActivity],
    no_color: bool,
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
) {
    let grouping = match state.grouping() {
        Grouping::Burst => "bursts",
        Grouping::Adjacent => "adjacent",
    };
    let block = section(
        format!("Journal · local · {grouping}{}", filter_status(state)),
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
            lines.push(tail_line(
                row,
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
    gutter: &'static str,
    selected: bool,
    show_host: bool,
    no_color: bool,
    width: usize,
) -> Line<'a> {
    let event = &row.first;
    let (severity, severity_style) = severity(event.priority, no_color);
    let first = crate::local_time::format(event.realtime_us);
    let stamp = if width < 65 {
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
    let short_unit = event
        .unit
        .strip_suffix(".service")
        .filter(|stem| !stem.is_empty())
        .unwrap_or(&event.unit);
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
) {
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
    let key = Line::from(vec![
        Span::styled("↑", rising_style(no_color)),
        Span::styled(" rising ", dim),
        spark_bar("▆", BinSeverity::Warning, dim, no_color),
        Span::styled(" warn ", dim),
        spark_bar("▆", BinSeverity::Error, dim, no_color),
        Span::styled(" err+", dim),
    ]);
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
    scroll: usize,
    no_color: bool,
) -> usize {
    let entry = &group.last;
    let host = entry.host.as_deref().unwrap_or("(unknown)");
    let cursor = entry.cursor.as_deref().unwrap_or("(missing)");
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
    let realtime = |timestamp: Option<u64>| {
        timestamp.map_or_else(|| "(missing)".to_owned(), |us| us.to_string())
    };
    if group.count == 1 {
        fields.push(format!(
            "Local {} · Journal realtime µs {}",
            crate::local_time::format(entry.realtime_us),
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
    let content = block.inner(area);
    let mut lines = Vec::new();
    for field in &fields {
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

fn render_footer(frame: &mut Frame<'_>, area: Rect, status: &Status<'_>, no_color: bool) {
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
        frame.render_widget(Line::raw(hint), keys);
        return;
    }
    let mut spans = Vec::new();
    let pane = if status.service_focus {
        "journal"
    } else {
        "services"
    };
    let separator = if area.width >= 88 { "  " } else { " " };
    let keys = [
        ("q", "quit"),
        ("Tab", pane),
        ("↑↓", "move"),
        ("↵", "view"),
        ("s", "solo"),
        ("m/M", "mute"),
        ("/", "find"),
        ("Space", "pause"),
        ("g", "live"),
    ];
    let mut remaining = usize::from(area.width);
    for (key, action) in keys {
        let width = key.width() + 1 + action.width();
        if width > remaining {
            break;
        }
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
    const LEVELS: [&str; 8] = ["▁", "▂", "▃", "▄", "▅", "▆", "▇", "█"];
    let counts = buckets(bins, bars).map(|bin| bin.iter().sum::<u64>());
    let max = peak.unwrap_or_else(|| counts.clone().max().unwrap_or(0));
    counts
        .zip(buckets(severity, bars))
        .map(move |(count, severities)| {
            let height = if count == 0 {
                "·"
            } else {
                let bar = (count as u128 * 7 / max as u128) as usize;
                LEVELS[bar]
            };
            spark_bar(
                height,
                severities.iter().copied().max().unwrap_or_default(),
                normal,
                no_color,
            )
        })
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
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| {
                render(frame, state, status, no_color);
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
