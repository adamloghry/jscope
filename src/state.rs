use crate::journal::Event;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;
use std::sync::Arc;

pub const WINDOW_MS: u64 = 60_000;
pub const SPARK_BINS: usize = 12;
const BIN_MS: u64 = WINDOW_MS / SPARK_BINS as u64;
const MAX_ROW_BYTES: usize = 12 * 1024 * 1024;
const MAX_ACTIVITY_BYTES: usize = 12 * 1024 * 1024;
const SAMPLE_LIMIT: usize = 6;
const MAX_MUTED_UNITS: usize = 128;
const MAX_MUTED_UNIT_BYTES: usize = 128 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UnitMuteLimit;

impl fmt::Display for UnitMuteLimit {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "Unit mute limit reached ({MAX_MUTED_UNITS} units or {} KiB of names)",
            MAX_MUTED_UNIT_BYTES / 1024
        )
    }
}

impl std::error::Error for UnitMuteLimit {}

#[derive(Debug)]
pub struct Group {
    pub id: u64,
    pub count: u64,
    pub first: Arc<Event>,
    pub last: Arc<Event>,
    times: VecDeque<Option<u64>>,
}

impl Group {
    pub fn recent_times(&self) -> &VecDeque<Option<u64>> {
        &self.times
    }

    fn bytes(&self) -> usize {
        let first = event_bytes(&self.first);
        if Arc::ptr_eq(&self.first, &self.last) {
            first
        } else {
            first + event_bytes(&self.last)
        }
    }

    fn push(&mut self, event: Arc<Event>) {
        self.count += 1;
        if self.times.len() == SAMPLE_LIMIT {
            self.times.pop_front();
        }
        self.times.push_back(event.realtime_us);
        self.last = event;
    }
}

#[derive(Debug)]
struct Intake {
    at: u64,
    event: Arc<Event>,
    count: u64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct UnitActivity {
    pub unit: String,
    pub incoming: u64,
    pub visible: u64,
    pub suppressed: u64,
    /// Counts in five-second bins, oldest first.
    pub sparkline: [u64; SPARK_BINS],
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Counters {
    pub received: u64,
    pub evicted_rows: u64,
    pub evicted_occurrences: u64,
    /// Occurrences removed from rate accounting before their window elapsed.
    pub activity_dropped: u64,
}

#[derive(Debug)]
pub struct State {
    rows: VecDeque<Group>,
    intake: VecDeque<Intake>,
    row_bytes: usize,
    activity_bytes: usize,
    max_rows: usize,
    max_activity: usize,
    now: u64,
    counters: Counters,
    first_host: Option<String>,
    multiple_hosts: bool,
    muted: bool,
    muted_units: BTreeSet<Box<str>>,
    solo: Option<String>,
    search: String,
    search_lower: String,
    paused: bool,
    paused_row: Option<u64>,
    selected_unit: Option<String>,
    expanded: Option<u64>,
    detail_scroll: usize,
}

impl Default for State {
    fn default() -> Self {
        Self::new(2_000, 16_384)
    }
}

impl State {
    pub fn new(max_rows: usize, max_activity: usize) -> Self {
        Self {
            rows: VecDeque::new(),
            intake: VecDeque::new(),
            row_bytes: 0,
            activity_bytes: 0,
            max_rows,
            max_activity,
            now: 0,
            counters: Counters::default(),
            first_host: None,
            multiple_hosts: false,
            muted: true,
            muted_units: BTreeSet::new(),
            solo: None,
            search: String::new(),
            search_lower: String::new(),
            paused: false,
            paused_row: None,
            selected_unit: None,
            expanded: None,
            detail_scroll: 0,
        }
    }

    pub fn advance(&mut self, now_ms: u64) {
        self.now = self.now.max(now_ms);
        while self
            .intake
            .front()
            .is_some_and(|entry| self.now - entry.at >= WINDOW_MS)
        {
            self.pop_intake();
        }
    }

    pub fn ingest(&mut self, event: Event, now_ms: u64) {
        self.advance(now_ms);
        self.counters.received += 1;
        if let Some(host) = &event.host {
            if let Some(first_host) = &self.first_host {
                self.multiple_hosts |= first_host != host;
            } else {
                self.first_host = Some(host.clone());
            }
        }

        let event = Arc::new(event);
        self.record_activity(Arc::clone(&event));
        self.record_row(event);
    }

    fn record_activity(&mut self, event: Arc<Event>) {
        if let Some(last) = self.intake.back_mut()
            && last.at == self.now
            && same_key(&last.event, &event)
        {
            last.count += 1;
            return;
        }

        self.activity_bytes += event_bytes(&event);
        self.intake.push_back(Intake {
            at: self.now,
            event,
            count: 1,
        });
        while self.intake.len() > self.max_activity || self.activity_bytes > MAX_ACTIVITY_BYTES {
            let removed = self.pop_intake();
            self.counters.activity_dropped += removed.count;
        }
    }

    fn pop_intake(&mut self) -> Intake {
        let removed = self.intake.pop_front().expect("an intake entry to remove");
        self.activity_bytes -= event_bytes(&removed.event);
        removed
    }

    fn record_row(&mut self, event: Arc<Event>) {
        if let Some(last) = self.rows.back_mut()
            && same_key(&last.first, &event)
        {
            self.row_bytes -= last.bytes();
            last.push(event);
            self.row_bytes += last.bytes();
        } else {
            self.row_bytes += event_bytes(&event);
            let realtime_us = event.realtime_us;
            self.rows.push_back(Group {
                id: self.counters.received,
                count: 1,
                first: Arc::clone(&event),
                last: event,
                times: VecDeque::from([realtime_us]),
            });
        }
        while self.rows.len() > self.max_rows || self.row_bytes > MAX_ROW_BYTES {
            let removed = self.rows.pop_front().expect("a row to remove");
            self.row_bytes -= removed.bytes();
            self.counters.evicted_rows += 1;
            self.counters.evicted_occurrences += removed.count;
            if self.expanded == Some(removed.id) {
                self.expanded = None;
            }
        }
    }

    pub fn rows(&self) -> &VecDeque<Group> {
        &self.rows
    }

    pub fn activity_len(&self) -> usize {
        self.intake.len()
    }

    pub fn counters(&self) -> Counters {
        self.counters
    }

    pub fn show_hosts(&self) -> bool {
        self.multiple_hosts
    }

    pub fn visible_rows(&self) -> impl DoubleEndedIterator<Item = &Group> {
        self.rows.iter().filter(|row| self.matches(&row.first))
    }

    pub fn activity(&self) -> Vec<UnitActivity> {
        self.summarize(false)
    }

    pub fn sidebar_activity(&self) -> Vec<UnitActivity> {
        self.summarize(true)
    }

    fn summarize(&self, include_muted_units: bool) -> Vec<UnitActivity> {
        let mut totals = BTreeMap::<&str, UnitActivity>::new();
        if include_muted_units {
            totals.extend(self.muted_units.iter().map(|unit| {
                (
                    unit.as_ref(),
                    UnitActivity {
                        unit: unit.to_string(),
                        ..Default::default()
                    },
                )
            }));
        }
        for entry in &self.intake {
            let unit = entry.event.unit.as_str();
            let summary = totals.entry(unit).or_insert_with(|| UnitActivity {
                unit: unit.to_owned(),
                ..Default::default()
            });
            let bin = SPARK_BINS - 1 - ((self.now - entry.at) / BIN_MS) as usize;
            summary.sparkline[bin] += entry.count;
            summary.incoming += entry.count;
            if self.matches(&entry.event) {
                summary.visible += entry.count;
            } else {
                summary.suppressed += entry.count;
            }
        }
        let mut totals: Vec<_> = totals.into_values().collect();
        totals.sort_unstable_by(|a, b| {
            b.incoming
                .cmp(&a.incoming)
                .then_with(|| a.unit.cmp(&b.unit))
        });
        totals
    }

    fn matches(&self, event: &Event) -> bool {
        (event.priority.is_none_or(|p| p <= 4) || !(self.muted || self.is_unit_muted(&event.unit)))
            && self.solo.as_ref().is_none_or(|unit| unit == &event.unit)
            && (self.search_lower.is_empty()
                || contains(&event.unit, &self.search_lower)
                || contains(&event.message, &self.search_lower)
                || event
                    .host
                    .as_deref()
                    .is_some_and(|host| contains(host, &self.search_lower)))
    }

    pub fn is_muted(&self) -> bool {
        self.muted
    }

    pub fn set_muted(&mut self, muted: bool) {
        self.muted = muted;
    }

    pub fn toggle_mute(&mut self) {
        self.set_muted(!self.muted);
    }

    pub fn is_unit_muted(&self, unit: &str) -> bool {
        self.muted_units.contains(unit)
    }

    pub fn set_unit_muted(&mut self, unit: &str, muted: bool) -> Result<(), UnitMuteLimit> {
        if !muted {
            self.muted_units.remove(unit);
            return Ok(());
        }
        if self.is_unit_muted(unit) {
            return Ok(());
        }
        let used_bytes = self
            .muted_units
            .iter()
            .map(|name| name.len())
            .sum::<usize>();
        if self.muted_units.len() >= MAX_MUTED_UNITS
            || unit.len() > MAX_MUTED_UNIT_BYTES - used_bytes
        {
            return Err(UnitMuteLimit);
        }
        self.muted_units.insert(unit.into());
        Ok(())
    }

    pub fn toggle_unit_mute(&mut self) -> Result<(), UnitMuteLimit> {
        let activity = self.sidebar_activity();
        if let Some(selected) = self.sidebar_selection(&activity) {
            self.set_unit_muted(&selected.unit, !self.is_unit_muted(&selected.unit))?;
        }
        Ok(())
    }

    pub fn solo(&self) -> Option<&str> {
        self.solo.as_deref()
    }

    pub fn set_solo(&mut self, unit: Option<String>) {
        self.solo = unit;
    }

    pub fn toggle_solo(&mut self) {
        let activity = self.sidebar_activity();
        if self.solo().is_some_and(|solo| {
            !activity
                .iter()
                .any(|entry| entry.unit == solo && entry.incoming > 0)
        }) {
            self.set_solo(None);
            return;
        }
        let selected = self
            .sidebar_selection(&activity)
            .map(|entry| entry.unit.clone());
        self.set_solo(if self.solo == selected {
            None
        } else {
            selected
        });
    }

    pub fn search(&self) -> &str {
        &self.search
    }

    pub fn set_search(&mut self, query: &str) {
        self.search.clear();
        self.search.push_str(query);
        self.search_lower = query.to_lowercase();
    }

    pub fn is_paused(&self) -> bool {
        self.paused
    }

    pub fn set_paused(&mut self, paused: bool) {
        if paused && !self.paused {
            let selected = self.visible_rows().next_back().map(|row| row.id);
            self.paused_row = selected;
        }
        if !paused {
            self.paused_row = None;
            self.expanded = None;
        }
        self.paused = paused;
    }

    pub fn toggle_pause(&mut self) {
        self.set_paused(!self.paused);
    }

    pub fn jump_live(&mut self) {
        self.set_paused(false);
    }

    pub fn selected_group(&self) -> Option<&Group> {
        let mut rows = self.visible_rows();
        if self.paused {
            let selected = self.paused_row?;
            rows.rfind(|row| row.id <= selected)
                .or_else(|| self.visible_rows().next())
        } else {
            rows.next_back()
        }
    }

    pub fn move_row(&mut self, delta: isize) {
        let row_ids: Vec<_> = self.visible_rows().map(|row| row.id).collect();
        if row_ids.is_empty() {
            return;
        }
        let current = self
            .selected_group()
            .and_then(|selected| row_ids.binary_search(&selected.id).ok())
            .unwrap_or(row_ids.len() - 1);
        let next = current.saturating_add_signed(delta).min(row_ids.len() - 1);
        self.paused = true;
        self.paused_row = Some(row_ids[next]);
        if self.paused_row != self.expanded {
            self.expanded = None;
        }
    }

    pub fn selected_unit(&self) -> Option<&str> {
        self.selected_unit.as_deref()
    }

    fn sidebar_selection<'a>(&self, activity: &'a [UnitActivity]) -> Option<&'a UnitActivity> {
        activity.get(self.sidebar_index(activity))
    }

    pub(crate) fn sidebar_index(&self, activity: &[UnitActivity]) -> usize {
        activity
            .iter()
            .position(|entry| Some(entry.unit.as_str()) == self.selected_unit())
            .unwrap_or(0)
    }

    pub fn move_unit(&mut self, delta: isize) {
        let activity = self.sidebar_activity();
        if activity.is_empty() {
            self.selected_unit = None;
            return;
        }
        let current = self.sidebar_index(&activity);
        let next = current.saturating_add_signed(delta).min(activity.len() - 1);
        self.selected_unit = Some(activity[next].unit.clone());
    }

    pub fn toggle_expanded(&mut self) {
        if let Some(selected) = self.selected_group().map(|row| row.id) {
            self.set_paused(true);
            self.detail_scroll = 0;
            self.expanded = if self.expanded == Some(selected) {
                None
            } else {
                Some(selected)
            };
        }
    }

    pub fn scroll_details(&mut self, delta: isize, limit: usize) {
        if self.expanded_group().is_some() {
            self.detail_scroll = self
                .detail_scroll
                .min(limit)
                .saturating_add_signed(delta)
                .min(limit);
        }
    }

    pub(crate) fn detail_scroll(&self) -> usize {
        self.detail_scroll
    }

    pub fn expanded_group(&self) -> Option<&Group> {
        self.selected_group()
            .filter(|row| Some(row.id) == self.expanded)
    }
}

fn same_key(a: &Event, b: &Event) -> bool {
    a.host == b.host && a.unit == b.unit && a.priority == b.priority && a.message == b.message
}

fn contains(value: &str, needle: &str) -> bool {
    value.contains(needle) || value.to_lowercase().contains(needle)
}

fn event_bytes(event: &Event) -> usize {
    // The entry/row caps cover their fixed-size storage separately.
    event.cursor.as_ref().map_or(0, String::capacity)
        + event.host.as_ref().map_or(0, String::capacity)
        + event.unit.capacity()
        + event.message.capacity()
        + event.unit_metadata.heap_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(unit: &str, priority: u8, message: &str, time: u64) -> Event {
        Event {
            realtime_us: Some(time),
            cursor: Some(time.to_string()),
            priority: Some(priority),
            host: Some("host".to_owned()),
            unit: unit.to_owned(),
            unit_metadata: Default::default(),
            message: message.to_owned(),
        }
    }

    #[test]
    fn pause_keeps_selection_at_anchor_while_intake_advances() {
        let mut state = State::default();
        state.ingest(event("api", 4, "first", 1), 0);
        state.set_paused(true);
        state.ingest(event("api", 4, "second", 2), 1);
        assert_eq!(state.selected_group().unwrap().first.message, "first");
        assert_eq!(state.activity()[0].incoming, 2);
        state.jump_live();
        assert_eq!(state.selected_group().unwrap().first.message, "second");
    }

    #[test]
    fn occurrence_view_is_bounded_and_keeps_endpoints() {
        let mut state = State::new(1, 1);
        for time in 0..100 {
            state.ingest(event("api", 4, "again", time), 0);
        }
        state.toggle_expanded();
        let group = state.expanded_group().unwrap();
        assert_eq!(group.count, 100);
        assert_eq!(group.first.realtime_us, Some(0));
        assert_eq!(group.last.realtime_us, Some(99));
        assert_eq!(group.recent_times().len(), SAMPLE_LIMIT);
        assert_eq!(state.activity()[0].incoming, 100);
        state.ingest(event("other", 5, "new", 101), 0);
        assert_eq!(state.counters().evicted_occurrences, 100);
        assert_eq!(state.counters().activity_dropped, 100);
        assert!(state.expanded_group().is_none());
    }

    #[test]
    fn expired_sidebar_selection_can_clear_solo_then_use_top_active_unit() {
        let mut state = State::default();
        state.ingest(event("api", 4, "first", 1), 0);
        state.ingest(event("db", 4, "second", 2), 100);
        state.move_unit(-1);
        state.toggle_solo();
        assert_eq!(state.solo(), Some("api"));
        state.advance(60_000);
        assert_eq!(state.activity()[0].unit, "db");
        state.toggle_solo();
        assert_eq!(state.solo(), None);
        state.toggle_solo();
        assert_eq!(state.solo(), Some("db"));
    }

    #[test]
    fn unit_mute_filters_only_lower_priorities_and_leaves_intake_unchanged() {
        let mut state = State::default();
        state.set_muted(false);
        state.ingest(event("api", 6, "polling", 1), 0);
        state.ingest(event("api", 4, "overload", 2), 0);
        let mut unknown = event("api", 6, "unknown level", 3);
        unknown.priority = None;
        state.ingest(unknown, 1);
        state.ingest(event("db", 6, "connected", 4), 2);

        state.toggle_unit_mute().unwrap();
        let activity = state.activity();
        assert_eq!(activity[0].unit, "api");
        assert_eq!((activity[0].incoming, activity[0].visible), (3, 2));
        assert_eq!((activity[1].incoming, activity[1].visible), (1, 1));
        assert_eq!(state.visible_rows().count(), 3);
        state.toggle_mute();
        assert_eq!(state.visible_rows().count(), 2);
        state.toggle_mute();
        assert_eq!(state.visible_rows().count(), 3);

        state.advance(WINDOW_MS + 2);
        assert_eq!(state.activity().len(), 0);
        assert_eq!(state.sidebar_activity()[0].unit, "api");
        state.toggle_unit_mute().unwrap();
        assert!(!state.is_unit_muted("api"));
        assert_eq!(state.visible_rows().count(), 4);
    }

    #[test]
    fn unit_mute_limits_preserve_existing_settings_and_allow_removal() {
        let mut state = State::default();
        for index in 0..MAX_MUTED_UNITS {
            state.set_unit_muted(&index.to_string(), true).unwrap();
        }
        assert_eq!(state.set_unit_muted("extra", true), Err(UnitMuteLimit));
        assert_eq!(state.set_unit_muted("0", true), Ok(()));
        state.set_unit_muted("0", false).unwrap();
        assert_eq!(state.set_unit_muted("extra", true), Ok(()));
        assert!(!state.is_unit_muted("0"));

        let mut state = State::default();
        let long_name = "x".repeat(MAX_MUTED_UNIT_BYTES);
        assert_eq!(state.set_unit_muted(&long_name, true), Ok(()));
        assert_eq!(state.set_unit_muted("extra", true), Err(UnitMuteLimit));
        state.set_unit_muted(&long_name, false).unwrap();
        assert_eq!(state.set_unit_muted("extra", true), Ok(()));
    }

    #[test]
    fn search_tracks_case_insensitive_visibility_without_changing_rank() {
        let mut state = State::default();
        state.ingest(event("api", 5, "Ready", 1), 100);
        state.ingest(event("api", 4, "NOT READY", 2), 150);
        state.ingest(event("db", 3, "not connected", 3), 200);
        state.set_search("rEaDy");
        assert_eq!(state.activity()[0].incoming, 2);
        assert_eq!(state.activity()[0].visible, 1);
        assert_eq!(state.visible_rows().count(), 1);
        state.toggle_mute();
        assert_eq!(state.activity()[0].visible, 2);
        state.advance(60_100);
        assert_eq!(state.activity()[0].incoming, 1);
        assert_eq!(state.rows().len(), 3);
    }
}
