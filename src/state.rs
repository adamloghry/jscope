mod context;
mod pattern;
mod recovery;

pub use context::Surroundings;

use crate::journal::{Event, UNKNOWN_UNIT};
use recovery::Recoveries;
use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;
use std::sync::Arc;

pub const WINDOW_MS: u64 = 60_000;
pub const SPARK_BINS: usize = 12;
const BIN_MS: u64 = WINDOW_MS / SPARK_BINS as u64;
const COMPARISON_WINDOW_MS: u64 = BIN_MS + WINDOW_MS;
const MIN_BASELINE_MS: u64 = 10_000;
const MIN_RISING_COUNT: u64 = 3;
const BURST_WINDOW_MS: u64 = 5_000;
const BURST_DISTANCE: usize = 8;
const MAX_ROW_BYTES: usize = 12 * 1024 * 1024;
const MAX_ACTIVITY_BYTES: usize = 12 * 1024 * 1024;
const MAX_REPEAT_RECEIPTS: usize = 4_096;
const MAX_REPEAT_BYTES: usize = 4 * 1024 * 1024;
const SAMPLE_LIMIT: usize = 6;
const MAX_MESSAGE_DRIVERS: usize = 3;
const MAX_PINS: usize = 2;
const MAX_MUTED_UNITS: usize = 128;
const MAX_MUTED_UNIT_BYTES: usize = 128 * 1024;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Grouping {
    #[default]
    Burst,
    Adjacent,
}

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
    last_id: u64,
    first_at: u64,
    last_at: u64,
    times: VecDeque<Option<u64>>,
}

impl Group {
    pub fn recent_times(&self) -> &VecDeque<Option<u64>> {
        &self.times
    }

    /// Whether this row continues a short run from the preceding journal row.
    pub fn continues_from(&self, prior: &Self) -> bool {
        self.id > prior.last_id
            && routine_source(&self.first)
            && routine_source(&prior.last)
            && same_source(&self.first, &prior.last)
            && self
                .first_at
                .checked_sub(prior.last_at)
                .is_some_and(|gap| gap <= BURST_WINDOW_MS)
            && within_journal_window(prior.last.realtime_us, self.first.realtime_us)
    }

    fn within_burst(&self, event: &Event, id: u64, now: u64) -> bool {
        id - self.id <= BURST_DISTANCE as u64
            && now - self.first_at <= BURST_WINDOW_MS
            && routine_source(event)
            && within_journal_window(self.first.realtime_us, event.realtime_us)
            && self
                .last
                .realtime_us
                .zip(event.realtime_us)
                .is_none_or(|(prev, next)| next >= prev)
    }

    fn bytes(&self) -> usize {
        let first = event_bytes(&self.first);
        if Arc::ptr_eq(&self.first, &self.last) {
            first
        } else {
            first + event_bytes(&self.last)
        }
    }

    fn push(&mut self, event: Arc<Event>, id: u64, now: u64) {
        self.count += 1;
        self.last_id = id;
        self.last_at = now;
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
    first_id: u64,
    /// History row of the first occurrence represented by this intake record.
    group_id: u64,
    event: Arc<Event>,
    count: u64,
}

/// Capture-time keys for one retained intake record. Tally zero precedes the graph.
struct DriverRecord<'a> {
    tally: usize,
    intake: &'a Intake,
    pattern: Cow<'a, str>,
}

impl DriverRecord<'_> {
    fn key(&self, patterns: bool) -> &str {
        if patterns {
            &self.pattern
        } else {
            &self.intake.event.message
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
pub enum BinSeverity {
    #[default]
    Normal,
    Warning,
    Error,
}

impl BinSeverity {
    fn from_priority(priority: Option<u8>) -> Self {
        match priority {
            Some(0..=3) => Self::Error,
            Some(4) => Self::Warning,
            _ => Self::Normal,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SpikeTarget {
    receipt_id: u64,
    group_id: u64,
    event: Arc<Event>,
}

impl SpikeTarget {
    fn retained(&self, rows: &VecDeque<Group>) -> bool {
        rows.binary_search_by_key(&self.group_id, |row| row.id)
            .is_ok()
    }
}

#[derive(Debug, Eq, PartialEq)]
pub struct MessageCount {
    pub message: String,
    pub count: u64,
    /// Count for the same message or pattern in a fully observed preceding interval.
    pub previous_count: Option<u64>,
    receipts: Vec<SpikeTarget>,
}

impl MessageCount {
    pub fn occurrences(&self) -> impl ExactSizeIterator<Item = &Event> {
        self.receipts.iter().map(|target| target.event.as_ref())
    }
}

#[derive(Debug, Default)]
pub struct SpikeBin {
    pub count: u64,
    pub severity: BinSeverity,
    /// The top incoming messages in the selected grouping, ordered by count then text.
    pub drivers: Vec<MessageCount>,
    alternate_drivers: Vec<MessageCount>,
    target: Option<SpikeTarget>,
}

impl SpikeBin {
    /// First incoming occurrence of the severity displayed by this bin.
    pub fn event(&self) -> Option<&Event> {
        self.target.as_ref().map(|target| target.event.as_ref())
    }

    fn add(&mut self, entry: &Intake) {
        let severity = BinSeverity::from_priority(entry.event.priority);
        if self.count == 0 || severity > self.severity {
            self.severity = severity;
            self.target = Some(SpikeTarget {
                receipt_id: entry.first_id,
                group_id: entry.group_id,
                event: Arc::clone(&entry.event),
            });
        }
        self.count += entry.count;
    }
}

/// A confirmed return to the pre-burst rate, measured on the intake clock.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Recovery {
    /// Time from the first rising sample to the start of the confirmed quiet stretch.
    pub duration_ms: u64,
    /// Time since that quiet stretch began, relative to the observation or snapshot.
    pub age_ms: u64,
}

#[derive(Debug)]
pub struct SpikeLane {
    pub unit: String,
    pub recovery: Option<Recovery>,
    pub bins: [SpikeBin; SPARK_BINS],
}

/// The inspected service and optional comparison share one frozen intake minute.
#[derive(Debug)]
pub struct SpikeSnapshot {
    pub unit: String,
    pub recovery: Option<Recovery>,
    pub bins: [SpikeBin; SPARK_BINS],
    pub selected: usize,
    pub peer: Option<SpikeLane>,
    /// Preserve each graph's screen position when switching the active service.
    pub peer_first: bool,
    pub patterns: bool,
    // Driver rank and position among its retained receipts, in arrival order.
    following: Option<(usize, usize)>,
}

pub struct MessageOccurrence<'a> {
    pub event: &'a Event,
    pub index: usize,
    pub retained: usize,
    pub total: u64,
}

impl SpikeSnapshot {
    pub fn occurrence(&self) -> Option<MessageOccurrence<'_>> {
        let (rank, index) = self.following?;
        let driver = &self.bins[self.selected].drivers[rank];
        Some(MessageOccurrence {
            event: &driver.receipts[index].event,
            index,
            retained: driver.receipts.len(),
            total: driver.count,
        })
    }
}

#[derive(Debug)]
struct SpikeTrail {
    targets: [Option<SpikeTarget>; SPARK_BINS],
    // Live starts one past the newest bin. A row older than the window is before bin zero.
    cursor: isize,
}

impl SpikeTrail {
    fn from_bins(bins: &[SpikeBin; SPARK_BINS], cursor: isize) -> Self {
        let targets = std::array::from_fn(|index| {
            let bin = &bins[index];
            if bin.severity == BinSeverity::Normal {
                None
            } else {
                bin.target.clone()
            }
        });
        Self { targets, cursor }
    }

    fn step(
        &mut self,
        direction: isize,
        rows: &VecDeque<Group>,
    ) -> Result<SpikeTarget, &'static str> {
        if direction == 0 {
            return Err("Choose an older or newer spike");
        }
        let step = direction.signum();
        let mut next = self.cursor + step;
        let mut evicted = false;
        while (0..SPARK_BINS as isize).contains(&next) {
            if let Some(target) = &self.targets[next as usize] {
                if target.retained(rows) {
                    self.cursor = next;
                    return Ok(target.clone());
                }
                evicted = true;
            }
            next += step;
        }
        Err(if evicted {
            "The journal rows for those spikes were evicted"
        } else if step < 0 {
            "No older spike in this minute"
        } else {
            "No newer spike in this minute"
        })
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct UnitActivity {
    pub unit: String,
    pub incoming: u64,
    pub visible: u64,
    pub suppressed: u64,
    /// Counts in five-second bins, oldest first.
    pub sparkline: [u64; SPARK_BINS],
    /// Highest incoming severity in each corresponding sparkline bin.
    pub severity: [BinSeverity; SPARK_BINS],
    /// The most recent five seconds have at least three arrivals and twice the baseline rate.
    pub rising: bool,
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
    grouping: Grouping,
    intake: VecDeque<Intake>,
    repeats: VecDeque<SpikeTarget>,
    repeat_bytes: usize,
    row_bytes: usize,
    activity_bytes: usize,
    max_rows: usize,
    max_activity: usize,
    now: u64,
    last_activity_drop_at: Option<u64>,
    recoveries: Recoveries,
    counters: Counters,
    first_host: Option<String>,
    multiple_hosts: bool,
    muted: bool,
    muted_units: BTreeSet<Box<str>>,
    pins: BTreeSet<Box<str>>,
    solo: Option<String>,
    search: String,
    search_lower: String,
    paused: bool,
    paused_row: Option<u64>,
    spike: Option<SpikeSnapshot>,
    spike_hit: Option<SpikeTarget>,
    spike_trail: Option<SpikeTrail>,
    context: Option<Surroundings>,
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
            grouping: Grouping::default(),
            intake: VecDeque::new(),
            repeats: VecDeque::new(),
            repeat_bytes: 0,
            row_bytes: 0,
            activity_bytes: 0,
            max_rows,
            max_activity,
            now: 0,
            last_activity_drop_at: None,
            recoveries: Recoveries::default(),
            counters: Counters::default(),
            first_host: None,
            multiple_hosts: false,
            muted: false,
            muted_units: BTreeSet::new(),
            pins: BTreeSet::new(),
            solo: None,
            search: String::new(),
            search_lower: String::new(),
            paused: false,
            paused_row: None,
            spike: None,
            spike_hit: None,
            spike_trail: None,
            context: None,
            selected_unit: None,
            expanded: None,
            detail_scroll: 0,
        }
    }

    pub fn advance(&mut self, now_ms: u64) {
        let now = self.now.max(now_ms);
        self.recoveries
            .advance(self.now, now, &self.intake, self.last_activity_drop_at);
        self.now = now;
        while self
            .intake
            .front()
            .is_some_and(|entry| self.now - entry.at >= COMPARISON_WINDOW_MS)
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
        let group_id = self.record_row(Arc::clone(&event));
        self.record_activity(event, group_id);
    }

    fn record_activity(&mut self, event: Arc<Event>, group_id: u64) {
        if let Some(last) = self.intake.back_mut()
            && last.at == self.now
            && same_key(&last.event, &event)
        {
            last.count += 1;
            self.repeat_bytes += event_bytes(&event);
            self.repeats.push_back(SpikeTarget {
                receipt_id: self.counters.received,
                group_id,
                event,
            });
            while self.repeats.len() > MAX_REPEAT_RECEIPTS || self.repeat_bytes > MAX_REPEAT_BYTES {
                self.pop_repeat();
            }
            return;
        }

        self.activity_bytes += event_bytes(&event);
        self.intake.push_back(Intake {
            at: self.now,
            first_id: self.counters.received,
            group_id,
            event,
            count: 1,
        });
        while self.intake.len() > self.max_activity || self.activity_bytes > MAX_ACTIVITY_BYTES {
            let removed = self.pop_intake();
            self.last_activity_drop_at = Some(removed.at);
            self.recoveries.clear();
            if self.now - removed.at < WINDOW_MS {
                self.counters.activity_dropped += removed.count;
            }
        }
    }

    fn pop_intake(&mut self) -> Intake {
        let removed = self.intake.pop_front().expect("an intake entry to remove");
        self.activity_bytes -= event_bytes(&removed.event);
        let last_id = removed.first_id + (removed.count - 1);
        while self
            .repeats
            .front()
            .is_some_and(|receipt| receipt.receipt_id <= last_id)
        {
            self.pop_repeat();
        }
        removed
    }

    fn pop_repeat(&mut self) {
        let receipt = self
            .repeats
            .pop_front()
            .expect("a repeat receipt to remove");
        self.repeat_bytes -= event_bytes(&receipt.event);
    }

    fn record_row(&mut self, event: Arc<Event>) -> u64 {
        let id = self.counters.received;
        // An interleaved match starts within the last eight events. Immediate repeats can
        // extend it indefinitely, but cannot insert any newer rows between those repeats.
        let repeat = self
            .rows
            .iter_mut()
            .rev()
            .take(BURST_DISTANCE)
            .find(|row| same_source(&row.last, &event))
            .filter(|row| {
                same_key(&row.last, &event)
                    && (id - row.last_id == 1
                        || (self.grouping == Grouping::Burst
                            && row.within_burst(&event, id, self.now)))
            });
        let group_id = if let Some(group) = repeat {
            self.row_bytes -= group.bytes();
            group.push(event, id, self.now);
            self.row_bytes += group.bytes();
            group.id
        } else {
            self.row_bytes += event_bytes(&event);
            let realtime_us = event.realtime_us;
            self.rows.push_back(Group {
                id,
                last_id: id,
                first_at: self.now,
                last_at: self.now,
                count: 1,
                first: Arc::clone(&event),
                last: event,
                times: VecDeque::from([realtime_us]),
            });
            id
        };
        while self.rows.len() > self.max_rows || self.row_bytes > MAX_ROW_BYTES {
            let removed = self.rows.pop_front().expect("a row to remove");
            self.row_bytes -= removed.bytes();
            self.counters.evicted_rows += 1;
            self.counters.evicted_occurrences += removed.count;
            if self.expanded == Some(removed.id) {
                self.expanded = None;
            }
            if self
                .spike_hit
                .as_ref()
                .is_some_and(|hit| hit.group_id == removed.id)
            {
                self.spike_hit = None;
            }
        }
        group_id
    }

    pub fn rows(&self) -> &VecDeque<Group> {
        &self.rows
    }

    pub fn grouping(&self) -> Grouping {
        self.grouping
    }

    /// Choose how future events are grouped. Existing rows retain their identities.
    pub fn set_grouping(&mut self, grouping: Grouping) {
        self.grouping = grouping;
    }

    /// Number of retained intake records contributing to the displayed 60-second window.
    pub fn activity_len(&self) -> usize {
        self.intake.len()
            - self
                .intake
                .partition_point(|entry| self.now - entry.at >= WINDOW_MS)
    }

    pub fn counters(&self) -> Counters {
        self.counters
    }

    pub fn show_hosts(&self) -> bool {
        self.multiple_hosts
    }

    pub fn visible_rows(&self) -> impl DoubleEndedIterator<Item = &Group> {
        self.indexed_visible_rows().map(|(_, group)| group)
    }

    pub(crate) fn indexed_visible_rows(&self) -> impl DoubleEndedIterator<Item = (usize, &Group)> {
        self.rows.iter().enumerate().filter(|(_, row)| {
            self.spike_hit
                .as_ref()
                .is_some_and(|hit| hit.group_id == row.id)
                || self.matches(&row.first)
        })
    }

    pub fn activity(&self) -> Vec<UnitActivity> {
        self.summarize(false)
    }

    pub fn recovery(&self, unit: &str) -> Option<Recovery> {
        self.recoveries.marker(unit, self.now)
    }

    pub fn sidebar_activity(&self) -> Vec<UnitActivity> {
        self.summarize(true)
    }

    fn summarize(&self, include_tracked_units: bool) -> Vec<UnitActivity> {
        let mut totals = BTreeMap::<&str, (UnitActivity, u64)>::new();
        if include_tracked_units {
            totals.extend(
                self.muted_units
                    .iter()
                    .chain(&self.pins)
                    .map(|unit| (unit.as_ref(), Default::default())),
            );
        }
        // Visit active units first. The extra five seconds only extend their rate baselines.
        for entry in self.intake.iter().rev() {
            let unit = entry.event.unit.as_str();
            let age = self.now - entry.at;
            let Some(bin) = activity_bin(age) else {
                if let Some((_, extra)) = totals.get_mut(unit) {
                    *extra += entry.count;
                }
                continue;
            };
            let (summary, _) = totals.entry(unit).or_default();
            summary.sparkline[bin] += entry.count;
            summary.severity[bin] =
                summary.severity[bin].max(BinSeverity::from_priority(entry.event.priority));
            summary.incoming += entry.count;
            if self.matches(&entry.event) {
                summary.visible += entry.count;
            } else {
                summary.suppressed += entry.count;
            }
        }
        let baseline_ms = self.now.saturating_sub(BIN_MS).min(WINDOW_MS);
        let baseline_complete = self
            .last_activity_drop_at
            .is_none_or(|at| self.now - at >= COMPARISON_WINDOW_MS);
        let mut totals: Vec<_> = totals
            .into_iter()
            .map(|(unit, (mut summary, extra))| {
                let recent = summary.sparkline[SPARK_BINS - 1];
                let baseline = summary.incoming - recent + extra;
                summary.rising = baseline_complete && rising_rate(recent, baseline, baseline_ms);
                summary.unit = unit.to_owned();
                summary
            })
            .collect();
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
        self.clear_spike_landing();
    }

    pub fn toggle_mute(&mut self) {
        self.set_muted(!self.muted);
    }

    pub fn is_unit_muted(&self, unit: &str) -> bool {
        self.muted_units.contains(unit)
    }

    pub fn set_unit_muted(&mut self, unit: &str, muted: bool) -> Result<(), UnitMuteLimit> {
        if !muted {
            if self.muted_units.remove(unit) {
                self.clear_spike_landing();
            }
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
        self.clear_spike_landing();
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
        self.clear_spike_landing();
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
        self.clear_spike_landing();
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
            self.clear_spike_landing();
        }
        self.paused = paused;
    }

    pub fn toggle_pause(&mut self) {
        self.set_paused(!self.paused);
    }

    pub fn jump_live(&mut self) {
        self.close_context();
        self.cancel_spike();
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

    /// Open the group that was actually painted, even if more entries have arrived.
    pub fn toggle_row_details(&mut self, id: u64) -> bool {
        if !self.visible_rows().any(|row| row.id == id) {
            return false;
        }
        if self.paused_row != Some(id) {
            self.clear_spike_landing();
        }
        self.paused = true;
        self.paused_row = Some(id);
        self.toggle_expanded();
        true
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
        let releasing_revealed_row =
            self.spike_hit_hidden() && self.paused_row == Some(row_ids[next]);
        self.clear_spike_landing();
        self.paused = true;
        self.paused_row = Some(row_ids[next]);
        if releasing_revealed_row {
            // A clamped move can still point to the row whose filter exception just ended.
            self.paused_row = self.selected_group().map(|row| row.id);
        }
        if self.paused_row != self.expanded {
            self.expanded = None;
        }
    }

    /// Wheel navigation rejoins the live tail when it reaches the newest visible row.
    pub fn scroll_tail(&mut self, delta: isize) {
        if delta > 0 && !self.paused {
            return;
        }
        self.move_row(delta);
        if delta > 0
            && self
                .visible_rows()
                .next_back()
                .is_some_and(|row| Some(row.id) == self.paused_row)
        {
            self.jump_live();
        }
    }

    pub fn selected_unit(&self) -> Option<&str> {
        self.selected_unit.as_deref()
    }

    pub fn is_pinned(&self, unit: &str) -> bool {
        self.pins.contains(unit)
    }

    pub fn toggle_pin(&mut self) -> Result<(), &'static str> {
        let activity = self.sidebar_activity();
        let unit = &self
            .sidebar_selection(&activity)
            .ok_or("No service to pin")?
            .unit;
        if self.pins.remove(unit.as_str()) {
            return Ok(());
        }
        self.pin_unit(unit)
    }

    pub fn pin_unit(&mut self, unit: &str) -> Result<(), &'static str> {
        if !self.is_pinned(unit) && self.pins.len() == MAX_PINS {
            return Err("Two services are pinned. Unpin one first");
        }
        self.pins.insert(unit.into());
        Ok(())
    }

    fn window_entries(&self) -> impl Iterator<Item = (usize, &Intake)> {
        self.intake
            .iter()
            .filter_map(|entry| activity_bin(self.now - entry.at).map(|index| (index, entry)))
    }

    fn capture_lane(&self, unit: &str) -> SpikeLane {
        let mut bins: [SpikeBin; SPARK_BINS] = Default::default();
        let mut records = Vec::new();
        for entry in self.intake.iter().filter(|entry| entry.event.unit == unit) {
            let tally = activity_bin(self.now - entry.at).map_or(0, |index| {
                bins[index].add(entry);
                index + 1
            });
            records.push(DriverRecord {
                tally,
                intake: entry,
                pattern: pattern::message_pattern(&entry.event.message),
            });
        }
        let exact = self.rank_drivers(&records, false);
        let patterns = self.rank_drivers(&records, true);
        for ((bin, drivers), alternate) in bins.iter_mut().zip(exact).zip(patterns) {
            bin.drivers = drivers;
            bin.alternate_drivers = alternate;
        }
        SpikeLane {
            unit: unit.to_owned(),
            recovery: self.recovery(unit),
            bins,
        }
    }

    fn rank_drivers(
        &self,
        records: &[DriverRecord<'_>],
        patterns: bool,
    ) -> [Vec<MessageCount>; SPARK_BINS] {
        // The first tally covers the extra five seconds before the visible graph.
        let mut messages: [BTreeMap<&str, u64>; SPARK_BINS + 1] = Default::default();
        for record in records {
            *messages[record.tally]
                .entry(record.key(patterns))
                .or_default() += record.intake.count;
        }
        let mut result: [Vec<MessageCount>; SPARK_BINS] = std::array::from_fn(|index| {
            let previous = &messages[index];
            let age_at_start = COMPARISON_WINDOW_MS - index as u64 * BIN_MS;
            let complete = self.now >= age_at_start
                && self
                    .last_activity_drop_at
                    .is_none_or(|at| self.now - at >= age_at_start);
            let mut ranked: Vec<_> = messages[index + 1].iter().collect();
            ranked.sort_unstable_by(|(a, n), (b, m)| m.cmp(n).then_with(|| a.cmp(b)));
            // The tally borrows intake text. Only the winners survive capture.
            ranked
                .into_iter()
                .take(MAX_MESSAGE_DRIVERS)
                .map(|(&message, &count)| MessageCount {
                    message: message.to_owned(),
                    count,
                    previous_count: complete.then(|| previous.get(message).copied().unwrap_or(0)),
                    receipts: Vec::new(),
                })
                .collect()
        });
        for record in records.iter().filter(|record| record.tally != 0) {
            let entry = record.intake;
            let Some(driver) = result[record.tally - 1]
                .iter_mut()
                .find(|driver| driver.message == record.key(patterns))
            else {
                continue;
            };
            driver.receipts.push(SpikeTarget {
                receipt_id: entry.first_id,
                group_id: entry.group_id,
                event: Arc::clone(&entry.event),
            });
            if entry.count > 1 {
                let start = self
                    .repeats
                    .partition_point(|receipt| receipt.receipt_id <= entry.first_id);
                driver.receipts.extend(
                    self.repeats
                        .range(start..)
                        .take_while(|receipt| receipt.receipt_id - entry.first_id < entry.count)
                        .cloned(),
                );
            }
        }
        result
    }

    pub fn begin_spike(&mut self) -> bool {
        let activity = self.sidebar_activity();
        let Some(selected) = self.sidebar_selection(&activity) else {
            self.spike = None;
            return false;
        };
        self.capture_spike(&selected.unit);
        true
    }

    /// Look up a clicked service by name as its intake ranking can change between draws.
    pub fn inspect_unit(&mut self, unit: &str) -> bool {
        if !self
            .sidebar_activity()
            .iter()
            .any(|entry| entry.unit == unit)
        {
            return false;
        }
        self.selected_unit = Some(unit.to_owned());
        self.capture_spike(unit);
        true
    }

    fn capture_spike(&mut self, unit: &str) {
        let SpikeLane {
            unit,
            bins,
            recovery,
        } = self.capture_lane(unit);
        let peer = if self.pins.len() < MAX_PINS || self.is_pinned(&unit) {
            self.pins
                .iter()
                .find(|pin| pin.as_ref() != unit)
                .map(|pin| self.capture_lane(pin))
        } else {
            None
        };
        // Highlight the latest severe bin. With routine traffic, prefer the largest burst.
        let index = bins
            .iter()
            .enumerate()
            .max_by_key(|(_, bin)| {
                let volume = if bin.severity == BinSeverity::Normal {
                    bin.count
                } else {
                    0
                };
                (bin.severity, volume)
            })
            .expect("a minute has at least one bin")
            .0;
        self.spike = Some(SpikeSnapshot {
            unit,
            recovery,
            bins,
            selected: index,
            peer,
            peer_first: false,
            patterns: false,
            following: None,
        });
    }

    pub fn spike(&self) -> Option<&SpikeSnapshot> {
        self.spike.as_ref()
    }

    pub fn move_spike(&mut self, delta: isize) {
        if let Some(spike) = &mut self.spike {
            spike.following = None;
            spike.selected = spike
                .selected
                .saturating_add_signed(delta)
                .min(SPARK_BINS - 1);
        }
    }

    /// Switch both lanes between the two rankings frozen at capture time.
    pub fn toggle_message_patterns(&mut self) {
        if let Some(spike) = &mut self.spike {
            spike.following = None;
            spike.patterns = !spike.patterns;
            let peer_bins = spike.peer.iter_mut().flat_map(|peer| &mut peer.bins);
            for bin in spike.bins.iter_mut().chain(peer_bins) {
                std::mem::swap(&mut bin.drivers, &mut bin.alternate_drivers);
            }
        }
    }

    pub fn switch_spike_unit(&mut self) {
        if let Some(spike) = &mut self.spike
            && let Some(peer) = &mut spike.peer
        {
            spike.following = None;
            std::mem::swap(&mut spike.unit, &mut peer.unit);
            std::mem::swap(&mut spike.bins, &mut peer.bins);
            std::mem::swap(&mut spike.recovery, &mut peer.recovery);
            spike.peer_first = !spike.peer_first;
        }
    }

    pub fn cancel_spike(&mut self) {
        self.spike = None;
    }

    pub fn follow_message(&mut self, rank: usize) -> Result<(), &'static str> {
        let spike = self
            .spike
            .as_mut()
            .ok_or("Select a service interval first")?;
        if rank >= spike.bins[spike.selected].drivers.len() {
            return Err("No message at that rank in this interval");
        }
        spike.following = Some((rank, 0));
        self.detail_scroll = 0;
        Ok(())
    }

    pub fn move_occurrence(&mut self, delta: isize) {
        if let Some(spike) = &mut self.spike
            && let Some((rank, index)) = &mut spike.following
        {
            let last = spike.bins[spike.selected].drivers[*rank].receipts.len() - 1;
            *index = index.saturating_add_signed(delta).min(last);
            self.detail_scroll = 0;
        }
    }

    pub fn leave_message(&mut self) {
        if let Some(spike) = &mut self.spike {
            spike.following = None;
        }
    }

    pub fn jump_to_spike(&mut self) -> Result<(), &'static str> {
        let spike = self
            .spike
            .as_ref()
            .ok_or("Select a service interval first")?;
        let bin = &spike.bins[spike.selected];
        let target = match spike.following {
            Some((rank, index)) => &bin.drivers[rank].receipts[index],
            None => bin.target.as_ref().ok_or("This interval is empty")?,
        };
        if !target.retained(&self.rows) {
            return Err("The journal row for this spike was evicted");
        }
        self.spike_trail = Some(SpikeTrail::from_bins(&spike.bins, spike.selected as isize));
        self.land_spike(target.clone());
        self.spike = None;
        Ok(())
    }

    fn global_spike_trail(&self) -> SpikeTrail {
        let mut bins: [SpikeBin; SPARK_BINS] = Default::default();
        for (index, entry) in self.window_entries() {
            bins[index].add(entry);
        }
        let cursor = if self.paused {
            self.selected_group()
                .and_then(|row| activity_bin(self.now - row.first_at))
                .map_or(-1, |index| index as isize)
        } else {
            SPARK_BINS as isize
        };
        SpikeTrail::from_bins(&bins, cursor)
    }

    /// Step between warning/error bins in a captured minute. From the live tail,
    /// capture all services. After an inspector jump, stay on its active service.
    pub fn hop_spike(&mut self, direction: isize) -> Result<(), &'static str> {
        let was_browsing = self.spike_trail.is_some();
        let mut trail = self
            .spike_trail
            .take()
            .unwrap_or_else(|| self.global_spike_trail());
        let target = trail.step(direction, &self.rows);
        // An unsuccessful first hop leaves a live tail ready to capture fresh arrivals.
        if was_browsing || target.is_ok() {
            self.spike_trail = Some(trail);
        }
        self.land_spike(target?);
        Ok(())
    }

    /// First press lands on the newest retained activation. Press again to move older.
    pub fn jump_generation(&mut self) -> Result<(), &'static str> {
        let before = self
            .spike_hit
            .as_ref()
            .filter(|hit| hit.event.generation().is_some())
            .map(|hit| hit.group_id);
        let row = self
            .rows
            .iter()
            .rev()
            .find(|row| before.is_none_or(|id| row.id < id) && row.first.generation().is_some())
            .ok_or(if before.is_some() {
                "No earlier generation marker retained"
            } else {
                "No generation marker retained"
            })?;
        let target = SpikeTarget {
            receipt_id: row.id,
            group_id: row.id,
            event: Arc::clone(&row.first),
        };
        self.spike_trail = None;
        self.land_spike(target);
        Ok(())
    }

    fn land_spike(&mut self, target: SpikeTarget) {
        self.paused = true;
        self.paused_row = Some(target.group_id);
        self.expanded = None;
        self.detail_scroll = 0;
        self.spike_hit = Some(target);
    }

    fn clear_spike_landing(&mut self) {
        self.spike_hit = None;
        self.spike_trail = None;
    }

    /// The exact occurrence selected in the graph, even inside a repeat group.
    pub fn spike_hit(&self) -> Option<&Event> {
        self.spike_hit.as_ref().map(|hit| hit.event.as_ref())
    }

    pub fn spike_hit_hidden(&self) -> bool {
        self.spike_hit().is_some_and(|event| !self.matches(event))
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
        if self
            .spike
            .as_ref()
            .is_some_and(|spike| spike.following.is_some())
            || self.expanded_group().is_some()
        {
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

fn rising_rate(recent: u64, baseline: u64, baseline_ms: u64) -> bool {
    baseline_ms >= MIN_BASELINE_MS
        && recent >= MIN_RISING_COUNT
        && u128::from(recent) * u128::from(baseline_ms)
            >= 2 * u128::from(baseline) * u128::from(BIN_MS)
}

fn activity_bin(age: u64) -> Option<usize> {
    (age < WINDOW_MS).then(|| SPARK_BINS - 1 - (age / BIN_MS) as usize)
}

fn same_key(a: &Event, b: &Event) -> bool {
    a.generation().is_none()
        && b.generation().is_none()
        && same_source(a, b)
        && a.priority == b.priority
        && a.message == b.message
}

fn same_source(a: &Event, b: &Event) -> bool {
    a.host == b.host && a.unit == b.unit
}

fn routine_source(event: &Event) -> bool {
    matches!(event.priority, Some(5..=7)) && event.unit != UNKNOWN_UNIT
}

fn within_journal_window(earlier: Option<u64>, later: Option<u64>) -> bool {
    earlier.zip(later).is_none_or(|(earlier, later)| {
        later
            .checked_sub(earlier)
            .is_some_and(|gap| gap <= BURST_WINDOW_MS * 1_000)
    })
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
        + event.generation().map_or(0, |g| {
            size_of_val(g) + g.current.capacity() + g.previous.as_ref().map_or(0, String::capacity)
        })
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
            generation: None,
            message: message.to_owned(),
        }
    }

    #[test]
    fn pointer_selection_tracks_ids_across_reranking_and_eviction() {
        let mut state = State::new(2, 16);
        state.ingest(event("a", 4, "first", 0), 0);
        let first = state.rows()[0].id;
        state.ingest(event("b", 4, "second", 1), 1);
        let second = state.rows()[1].id;
        state.ingest(event("b", 4, "third", 2), 2);
        assert!(!state.toggle_row_details(first));
        assert!(!state.is_paused());
        assert!(state.toggle_row_details(second));
        assert_eq!(state.expanded_group().unwrap().first.message, "second");
        state.scroll_tail(3);
        assert!(!state.is_paused());
        assert!(state.expanded_group().is_none());
        assert_eq!(state.sidebar_activity()[0].unit, "b");
        assert!(state.inspect_unit("a"));
        assert_eq!(state.spike().unwrap().unit, "a");
        state.cancel_spike();
        state.advance(WINDOW_MS + 2);
        assert!(!state.inspect_unit("a"));
        assert!(state.spike().is_none());
    }

    #[test]
    fn quiet_pins_remain_selectable_and_comparisons_exclude_unpinned_third_units() {
        let mut state = State::default();
        assert!(state.toggle_pin().is_err());
        for unit in ["a", "b", "c"] {
            state.ingest(event(unit, 4, "warning", 0), 0);
        }
        state.toggle_pin().unwrap();
        state.begin_spike();
        assert!(state.spike().unwrap().peer.is_none());
        state.cancel_spike();
        state.move_unit(1);
        state.begin_spike();
        assert_eq!(state.spike().unwrap().peer.as_ref().unwrap().unit, "a");
        state.cancel_spike();
        state.toggle_pin().unwrap();
        state.move_unit(1);
        assert!(state.toggle_pin().is_err());
        state.begin_spike();
        assert!(state.spike().unwrap().peer.is_none());
        state.cancel_spike();

        state.advance(WINDOW_MS);
        assert!(state.activity().is_empty());
        let quiet = state.sidebar_activity();
        assert_eq!(
            quiet.iter().map(|u| u.unit.as_str()).collect::<Vec<_>>(),
            ["a", "b"]
        );
        state.move_unit(1);
        state.begin_spike();
        let spike = state.spike().unwrap();
        assert_eq!(spike.unit, "b");
        assert_eq!(spike.peer.as_ref().unwrap().unit, "a");
        assert!(
            spike
                .bins
                .iter()
                .all(|bin| bin.count == 0 && bin.drivers.is_empty())
        );
        state.cancel_spike();
        state.toggle_pin().unwrap();
        assert_eq!(state.sidebar_activity().len(), 1);
        assert!(state.is_pinned("a"));
    }

    #[test]
    fn manual_tail_hops_use_first_receipt_and_report_evicted_only_candidates() {
        let mut state = State::new(3, 16);
        state.ingest(event("a", 4, "expired row", 999), 0);
        state.ingest(event("a", 4, "old warning", 80), 5_000);
        state.ingest(event("a", 6, "selected row", 30), 10_000);
        state.ingest(event("b", 3, "error", 10), 15_000);
        state.move_row(-1);
        assert_eq!(
            state.selected_group().unwrap().first.message,
            "selected row"
        );
        state.hop_spike(-1).unwrap();
        assert_eq!(state.spike_hit().unwrap().message, "old warning");
        assert!(state.hop_spike(-1).unwrap_err().contains("evicted"));
        assert_eq!(state.spike_hit().unwrap().message, "old warning");
        state.hop_spike(1).unwrap();
        assert_eq!(state.spike_hit().unwrap().message, "error");
        assert!(state.hop_spike(1).is_err());
    }

    #[test]
    fn manual_tail_hops_from_an_older_row_start_before_the_retained_minute() {
        let mut state = State::default();
        state.ingest(event("worker", 4, "before minute", 80), 0);
        state.ingest(event("worker", 6, "routine", 40), 10_000);
        state.ingest(event("worker", 4, "first marked interval", 20), 15_000);
        state.advance(WINDOW_MS);
        state.move_row(isize::MIN);
        assert!(state.hop_spike(-1).unwrap_err().contains("No older"));
        state.hop_spike(1).unwrap();
        assert_eq!(state.spike_hit().unwrap().message, "first marked interval");
        state.jump_live();
        state.advance(80_000);
        assert!(state.hop_spike(-1).is_err());
        state.ingest(event("worker", 3, "fresh error", 10), 80_001);
        state.hop_spike(-1).unwrap();
        assert_eq!(state.spike_hit().unwrap().message, "fresh error");
    }

    #[test]
    fn tail_paused_before_the_first_receipt_can_hop_forward_to_a_hidden_warning() {
        let mut state = State::default();
        state.set_paused(true);
        state.set_search("absent");
        state.ingest(event("worker", 4, "warning", 1), 2);
        assert!(state.hop_spike(-1).is_err());
        state.hop_spike(1).unwrap();
        assert!(state.spike_hit_hidden());
        assert_eq!(state.spike_hit().unwrap().message, "warning");
    }

    #[test]
    fn spike_defaults_to_latest_severe_bin_or_busiest_routine_bin() {
        let mut state = State::default();
        assert!(!state.begin_spike());
        state.set_unit_muted("worker", true).unwrap();
        assert!(state.begin_spike());
        assert_eq!(state.spike().unwrap().selected, 11);
        assert!(state.jump_to_spike().unwrap_err().contains("empty"));

        let e = event("worker", 6, "routine", 0);
        for _ in 0..4 {
            state.ingest(e.clone(), 0);
        }
        state.ingest(e.clone(), 5_000);
        state.begin_spike();
        assert_eq!(state.spike().unwrap().selected, 10);
        for _ in 0..3 {
            state.ingest(e.clone(), 5_000);
        }
        state.begin_spike();
        assert_eq!(state.spike().unwrap().selected, 11);
        state.ingest(event("worker", 4, "warning", 1), 6_000);
        state.ingest(e, 20_000);
        state.begin_spike();
        assert_eq!(state.spike().unwrap().selected, 9);
        state.ingest(event("worker", 0, "emergency", 2), 21_000);
        state.ingest(event("worker", 3, "error", 3), 27_000);
        state.ingest(event("worker", 4, "warning", 4), 33_000);
        state.begin_spike();
        assert_eq!(state.spike().unwrap().selected, 10);
        state.move_spike(isize::MIN);
        assert_eq!(state.spike().unwrap().selected, 0);
        state.move_spike(isize::MAX);
        assert_eq!(state.spike().unwrap().selected, 11);
        state.cancel_spike();
        assert!(state.spike().is_none());
    }

    #[test]
    fn spike_retains_first_occurrence_in_its_bin_after_live_data_expires() {
        let mut state = State::default();
        // One repeat group spans two bins. Two copies at the same intake millisecond
        // are one intake record, whose first cursor must remain the jump target.
        state.ingest(event("api", 3, "retry", 10), 0);
        state.ingest(event("api", 3, "retry", 5), 6_000);
        state.ingest(event("api", 3, "retry", 20), 6_000);
        state.ingest(event("api", 1, "worse", 30), 6_001);
        state.set_search("anything else");
        let before = state.activity();
        state.begin_spike();
        assert_eq!(state.spike().unwrap().bins[11].count, 3);
        assert_eq!(
            state.spike().unwrap().bins[11]
                .event()
                .unwrap()
                .cursor
                .as_deref(),
            Some("5")
        );
        state.advance(100_000);
        assert!(state.activity().is_empty());
        state.jump_to_spike().unwrap();
        assert!(state.spike_hit_hidden());
        assert!(state.is_paused());
        assert!(state.expanded_group().is_none());
        assert_eq!(state.selected_group().unwrap().count, 3);
        assert_eq!(state.spike_hit().unwrap().cursor.as_deref(), Some("5"));
        state.toggle_expanded();
        assert_eq!(
            state.expanded_group().unwrap().last.cursor.as_deref(),
            Some("20")
        );
        assert!(state.spike_hit_hidden());
        state.jump_live();
        assert_eq!(state.visible_rows().count(), 0);
        assert!(state.spike_hit().is_none());
        assert_eq!(state.search(), "anything else");
        assert_eq!(before[0].visible, 0);
    }

    #[test]
    fn landed_event_leaves_tail_after_navigation_filter_edit_or_eviction() {
        let mut state = State::new(3, 16);
        state.ingest(event("api", 4, "target", 1), 0);
        state.ingest(event("api", 6, "context", 2), 1);
        state.set_search("context");
        for action in ["navigate", "search", "solo", "mute", "evict"] {
            state.begin_spike();
            state.jump_to_spike().unwrap();
            assert!(state.spike_hit_hidden());
            assert_eq!(state.visible_rows().count(), 2);
            assert_eq!(state.activity()[0].visible, 1);
            match action {
                "navigate" => state.move_row(1),
                "search" => state.set_search("CONTEXT"),
                "solo" => state.set_solo(Some("api".into())),
                "mute" => state.set_unit_muted("other", true).unwrap(),
                _ => {
                    state.ingest(event("other", 4, "new", 3), 2);
                    state.ingest(event("other", 4, "newest", 4), 3);
                }
            }
            assert!(state.spike_hit().is_none());
            assert_eq!(state.selected_group().unwrap().first.message, "context");
        }
    }

    #[test]
    fn leaving_a_revealed_row_at_the_boundary_reanchors_to_visible_history() {
        let mut state = State::default();
        state.ingest(event("api", 4, "hit", 1), 1);
        state.ingest(event("api", 6, "context", 2), 2);
        state.set_search("context");
        state.begin_spike();
        state.jump_to_spike().unwrap();
        state.toggle_expanded();
        state.move_row(-1);
        state.set_search("");
        assert!(state.spike_hit().is_none());
        assert!(state.expanded_group().is_none());
        assert_eq!(state.selected_group().unwrap().id, 2);
    }

    #[test]
    fn rising_requires_a_warmed_baseline_three_arrivals_and_twice_the_rate() {
        let mut state = State::default();
        let e = event("api", 6, "poll", 0);
        for _ in 0..4 {
            state.ingest(e.clone(), 0);
        }
        for _ in 0..3 {
            state.ingest(e.clone(), 14_999);
        }
        assert!(!state.activity()[0].rising);
        state.advance(15_000);
        assert!(!state.activity()[0].rising);
        state.ingest(e.clone(), 15_000);
        // Four in five seconds, four in the preceding ten observed seconds.
        assert!(state.activity()[0].rising);
        state.set_search("absent");
        state.set_muted(true);
        state.set_paused(true);
        assert!(state.activity()[0].rising);
        state.advance(19_999);
        assert!(!state.activity()[0].rising);

        let mut quiet = State::default();
        for _ in 0..2 {
            quiet.ingest(e.clone(), 15_000);
        }
        assert!(!quiet.activity()[0].rising);
        quiet.ingest(e, 15_000);
        assert!(quiet.activity()[0].rising);
    }

    #[test]
    fn recovery_through_silence_requires_sustained_quiet_and_expires() {
        let mut state = State::default();
        let entry = event("api", 6, "poll", 0);
        for _ in 0..3 {
            state.ingest(entry.clone(), 16_000);
        }
        state.set_muted(true);
        state.set_paused(true);
        state.set_search("unmatched");
        // The detector observes the rise at 17s and quiet at 22s. One new
        // occurrence interrupts confirmation, even though it cannot start a rise.
        state.ingest(entry.clone(), 24_000);
        state.advance(34_999);
        assert_eq!(state.recovery("api").map(|r| r.duration_ms), None);
        state.advance(35_000);
        assert_eq!(state.recovery("api").map(|r| r.duration_ms), Some(13_000));
        for _ in 0..8 {
            state.ingest(entry.clone(), 40_000);
        }
        state.advance(41_000);
        assert_eq!(state.recovery("api").map(|r| r.duration_ms), None);
        state.advance(80_999);
        assert_eq!(state.recovery("api").map(|r| r.duration_ms), Some(5_000));
        state.advance(81_000);
        assert_eq!(state.recovery("api").map(|r| r.duration_ms), None);
    }

    #[test]
    fn a_falling_rate_above_the_pre_spike_average_stays_noisy() {
        let mut state = State::default();
        let entry = event("api", 6, "work", 0);
        for second in 0..60 {
            state.ingest(entry.clone(), second * 1_000);
        }
        for second in 60..140 {
            let rate = if second < 100 {
                4
            } else if second % 2 == 0 {
                2
            } else {
                1
            };
            for _ in 0..rate {
                state.ingest(entry.clone(), second * 1_000);
            }
        }
        assert!(!state.activity()[0].rising);
        assert_eq!(state.recovery("api").map(|r| r.duration_ms), None);
        state.advance(150_000);
        assert!(state.recovery("api").map(|r| r.duration_ms).unwrap() >= 75_000);
    }

    #[test]
    fn startup_and_capacity_loss_do_not_manufacture_recoveries() {
        let mut state = State::new(20, 2);
        let entry = event("api", 6, "poll", 0);
        for _ in 0..5 {
            state.ingest(entry.clone(), 0);
        }
        state.advance(15_000);
        assert_eq!(state.recovery("api").map(|r| r.duration_ms), None);
        for _ in 0..10 {
            state.ingest(entry.clone(), 16_000);
        }
        state.advance(17_000);
        state.ingest(event("other", 6, "fill capacity", 0), 17_001);
        state.advance(30_000);
        assert_eq!(state.recovery("api").map(|r| r.duration_ms), None);
        state.advance(u64::MAX - 12_000);
        for _ in 0..3 {
            state.ingest(entry.clone(), u64::MAX - 12_000);
        }
        state.advance(u64::MAX);
        assert_eq!(state.recovery("api").map(|r| r.duration_ms), Some(5_000));
    }

    #[test]
    fn comparison_includes_history_beyond_the_displayed_minute() {
        let mut state = State::default();
        let e = event("api", 6, "poll", 0);
        for _ in 0..19 {
            state.ingest(e.clone(), 0);
        }
        state.ingest(event("idle", 4, "warn", 0), 0);
        for _ in 0..3 {
            state.ingest(e.clone(), 64_999);
        }
        let entries = state.activity();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].incoming, 3);
        assert!(!entries[0].rising);
        assert_eq!(state.activity_len(), 1);
        state.advance(65_000);
        assert!(state.activity()[0].rising);
        state.advance(69_999);
        assert!(!state.activity()[0].rising);
    }

    #[test]
    fn severity_bins_keep_the_most_urgent_arrival_under_display_filters() {
        let mut state = State::default();
        state.ingest(event("api", 4, "caution", 0), 0);
        for priority in [3, 4, 7] {
            state.ingest(event("api", priority, "event", 0), 5_000);
        }
        state.ingest(event("api", 5, "notice", 0), 10_000);
        state.set_solo(Some("db".into()));
        let summary = &state.activity()[0];
        assert_eq!((summary.incoming, summary.visible), (5, 0));
        assert_eq!(
            &summary.severity[SPARK_BINS - 3..],
            &[
                BinSeverity::Warning,
                BinSeverity::Error,
                BinSeverity::Normal
            ]
        );
        assert_eq!(&summary.sparkline[SPARK_BINS - 3..], &[1, 3, 1]);
        state.advance(60_000);
        assert!(!state.activity()[0].severity.contains(&BinSeverity::Warning));
        state.advance(65_000);
        assert_eq!(
            state.activity()[0].severity,
            [BinSeverity::Normal; SPARK_BINS]
        );
    }

    #[test]
    fn dropping_baseline_only_history_suspends_rate_arrows_until_it_expires() {
        let mut state = State::new(5, 2);
        let e = event("api", 6, "poll", 0);
        let start = u64::MAX - COMPARISON_WINDOW_MS;
        state.ingest(event("old", 6, "poll", 0), start);
        state.ingest(event("other", 6, "tick", 0), start + 60_000);
        for _ in 0..3 {
            state.ingest(e.clone(), u64::MAX - 1);
        }
        assert_eq!(state.counters().activity_dropped, 0);
        assert_eq!(state.intake.len(), 2);
        assert_eq!(state.activity()[0].incoming, 3);
        assert!(!state.activity()[0].rising);
        state.advance(u64::MAX);
        assert!(state.activity()[0].rising);
    }

    #[test]
    fn interleaved_bursts_require_both_clocks_and_a_recent_start() {
        let folds = |gap_ms, first_us, last_us, intervening, unit, priority| {
            let mut state = State::default();
            let mut first = event(unit, 6, "tick", 0);
            first.priority = priority;
            first.realtime_us = first_us;
            let mut last = first.clone();
            last.realtime_us = last_us;
            state.ingest(first, 0);
            // Count input positions even when the intervening warning itself collapses.
            for _ in 0..intervening {
                state.ingest(event("other", 4, "warning", 0), 0);
            }
            state.ingest(last, gap_ms);
            state.rows()[0].count == 2
        };
        assert!(folds(5_000, Some(10), Some(5_000_010), 7, "api", Some(5)));
        assert!(!folds(5_001, Some(10), Some(10), 7, "api", Some(6)));
        assert!(!folds(1, Some(10), Some(5_000_011), 7, "api", Some(6)));
        assert!(!folds(1, Some(10), Some(9), 1, "api", Some(6)));
        assert!(!folds(1, Some(10), Some(10), 8, "api", Some(6)));
        assert!(folds(1, None, Some(10), 1, "api", Some(7)));
        assert!(folds(1, Some(10), None, 1, "api", Some(7)));
        assert!(!folds(1, None, None, 1, UNKNOWN_UNIT, Some(6)));
        for priority in [None, Some(0), Some(4)] {
            assert!(!folds(1, None, None, 1, "api", priority));
        }
    }

    #[test]
    fn adjacent_repeats_follow_the_last_input_even_when_its_group_is_earlier() {
        let mut state = State::default();
        state.ingest(event("api", 6, "tick", 10), 0);
        // Start seven different rows; the first occurrence is just within reach.
        for unit in ["b", "c", "d", "e", "f", "g", "h"] {
            state.ingest(event(unit, 4, "warn", 20), 1);
        }
        state.ingest(event("api", 6, "tick", 30), 2);
        // The raw stream stays adjacent even beyond both clock and sequence limits.
        for _ in 0..20 {
            state.ingest(event("api", 6, "tick", 8_000_000), 8_000);
        }
        assert_eq!(state.rows()[0].count, 22);
        assert_eq!(state.rows()[0].last.realtime_us, Some(8_000_000));
        state.ingest(event("h", 4, "warn", 9_000_000), 9_000);
        assert_eq!(state.rows().len(), 9);
        assert_eq!(state.rows()[7].count, 1);
        assert_eq!(state.rows()[8].count, 1);
    }

    #[test]
    fn interleaved_matches_respect_each_sources_latest_message_and_timestamp() {
        let mut state = State::default();
        for (unit, text, time) in [
            ("api", "tick", 10),
            ("other", "a", 20),
            ("api", "tick", 30),
            ("other", "b", 40),
            // Within range of the first occurrence but older than its latest sample.
            ("api", "tick", 25),
            ("api", "progress", 50),
            ("other", "c", 60),
            ("api", "tick", 70),
        ] {
            state.ingest(event(unit, 6, text, time), 0);
        }
        assert_eq!(
            state
                .rows()
                .iter()
                .map(|row| (row.id, row.count))
                .collect::<Vec<_>>(),
            [(1, 2), (2, 1), (4, 1), (5, 1), (6, 1), (7, 1), (8, 1)]
        );
    }

    #[test]
    fn row_continuations_require_ordered_nearby_routine_events_from_one_source() {
        let mut state = State::default();
        for (unit, priority, text, journal_us, intake_ms) in [
            ("api", 6, "a", 0, 0),
            ("api", 7, "b", 5_000_000, 5_000),
            ("api", 6, "c", 4_000_000, 5_000),
            ("api", 6, "d", 4_100_000, 10_001),
            ("api", 4, "e", 4_100_005, 10_001),
            ("api", 5, "f", 4_100_006, 10_001),
            ("db", 5, "g", 4_100_006, 10_001),
        ] {
            state.ingest(event(unit, priority, text, journal_us), intake_ms);
        }
        let rows = state.rows();
        assert!(rows[1].continues_from(&rows[0]));
        for index in 2..rows.len() {
            assert!(!rows[index].continues_from(&rows[index - 1]));
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
        assert_eq!(state.activity()[0].visible, 2);
        assert_eq!(state.visible_rows().count(), 2);
        state.toggle_mute();
        assert_eq!(state.activity()[0].visible, 1);
        assert_eq!(state.visible_rows().count(), 1);
        state.advance(60_100);
        assert_eq!(state.activity()[0].incoming, 1);
        assert_eq!(state.rows().len(), 3);
    }
}
