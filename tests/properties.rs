use hegel::{TestCase, generators as gs};
use jscope::{
    journal::{Decoder, Event, Frame, UnitField, UnitMetadata},
    state::{BinSeverity, Grouping, State, UnitActivity},
};
use serde_json::json;
use std::{collections::BTreeMap, io::BufReader};

const LINE_CAP: usize = 256;
const WINDOW: u64 = 60_000;
const UNITS: [&str; 3] = ["indexer.service", "proxy.service", "worker.service"];
const HOSTS: [Option<&str>; 3] = [None, Some("box"), Some("REMOTE")];
const MESSAGES: [&str; 3] = ["WORK ITEM", "queue behind", "link restored"];
const QUERIES: [&str; 5] = ["", "IndexER", "work item", "remote", "absent"];

#[hegel::test(test_cases = 200)]
fn generated_json_lines_match_the_stream_model(tc: TestCase) {
    let chunk = tc.draw(gs::integers::<usize>().min_value(1).max_value(80));
    let trailing_newline = tc.draw(gs::booleans());
    let records = tc.draw(
        gs::vecs(hegel::tuples!(
            gs::integers::<u8>().max_value(6),
            gs::integers::<usize>().max_value(450)
        ))
        .max_size(48),
    );

    let mut stream = Vec::new();
    let mut expected = Vec::new();
    for (index, (variant, number)) in records.into_iter().enumerate() {
        if index > 0 {
            stream.push(b'\n');
        }
        let (record, frame) = journal_record(variant, number);
        stream.extend(record);
        expected.push(frame);
    }
    if trailing_newline && !stream.is_empty() {
        stream.push(b'\n');
    }

    let mut decoder = Decoder::new(BufReader::with_capacity(chunk, stream.as_slice()), LINE_CAP);
    for frame in expected {
        assert_eq!(decoder.next_frame().unwrap(), Some(frame));
    }
    assert_eq!(decoder.next_frame().unwrap(), None);
}

fn journal_record(variant: u8, number: usize) -> (Vec<u8>, Frame) {
    let mut event = Event {
        realtime_us: None,
        cursor: None,
        priority: None,
        host: None,
        unit: "(unknown)".to_owned(),
        unit_metadata: UnitMetadata::default(),
        message: String::new(),
    };
    let fields = match variant {
        0 => return (b"{malformed".to_vec(), Frame::Malformed),
        1 => return (b"{\"MESSAGE\":\"\xff\"}".to_vec(), Frame::Malformed),
        2 => {
            // Include valid JSON at the byte limit and beyond it.
            event.message = "x".repeat(number);
            let fields = json!({ "MESSAGE": event.message });
            let line = fields.to_string().into_bytes();
            let frame = if line.len() <= LINE_CAP {
                Frame::Event(event)
            } else {
                Frame::Oversized
            };
            return (line, frame);
        }
        3 => {
            let priority = (number % 8) as u8;
            event.realtime_us = Some(number as u64);
            event.cursor = Some(format!("cursor-{number}"));
            event.priority = Some(priority);
            event.host = Some("box".to_owned());
            event.unit = "user.service".to_owned();
            event.unit_metadata = UnitMetadata::new(
                UnitField::UserUnit,
                Box::new([(UnitField::SystemUnit, "system.service".into())]),
            );
            event.message = "visible \\x1b[2J\\n\\\\\\u{9b}\\u{202e}".to_owned();
            json!({
                "__REALTIME_TIMESTAMP": [null, "bad", number.to_string()],
                "__CURSOR": event.cursor,
                "_HOSTNAME": [98, 111, 120],
                "_SYSTEMD_USER_UNIT": ["", "user.service", "ignored.service"],
                "_SYSTEMD_UNIT": "system.service",
                "PRIORITY": [null, priority.to_string(), "2"],
                "MESSAGE": "visible \u{1b}[2J\n\\\u{9b}\u{202e}",
            })
        }
        4 => {
            event.message = "A\\x00\\xff".to_owned();
            event.unit = "legacy".to_owned();
            event.unit_metadata = UnitMetadata::new(
                UnitField::SyslogIdentifier,
                Box::new([(UnitField::Command, "ignored".into())]),
            );
            json!({
                "_SYSTEMD_UNIT": null,
                "SYSLOG_IDENTIFIER": "legacy",
                "_COMM": "ignored",
                "MESSAGE": [[65, 0, 255], "ignored"],
            })
        }
        5 => {
            event.message = "\\(message unavailable)".to_owned();
            json!({ "PRIORITY": "9", "MESSAGE": null, "_SYSTEMD_UNIT": [] })
        }
        _ => {
            let prefix = b"{\"MESSAGE\":\"";
            let suffix = b"\"}";
            let len = LINE_CAP - prefix.len() - suffix.len() + number % 3;
            event.message = "x".repeat(len);
            let line = format!("{{\"MESSAGE\":\"{}\"}}", event.message).into_bytes();
            let frame = if line.len() <= LINE_CAP {
                Frame::Event(event)
            } else {
                Frame::Oversized
            };
            return (line, frame);
        }
    };
    let line = fields.to_string().into_bytes();
    assert!(
        line.len() <= LINE_CAP,
        "test record unexpectedly exceeds cap"
    );
    (line, Frame::Event(event))
}

#[derive(Clone, Copy, Debug)]
enum JournalClock {
    Forward,
    Backward,
    Missing,
}

#[derive(Debug)]
enum Action {
    Burst {
        count: usize,
        unit: usize,
        host: usize,
        message: usize,
        priority: Option<u8>,
        step_ms: u64,
        interleaved: bool,
        clock: JournalClock,
    },
    Advance(u64),
    Mute(bool),
    UnitMute(usize, bool),
    Solo(Option<&'static str>),
    Search(&'static str),
    Pause(bool),
    JumpLive,
}

#[hegel::composite]
fn action(tc: &TestCase) -> Action {
    let variant = tc.draw(gs::integers::<u8>().max_value(12));
    let interleaved = variant == 12;
    let choice = || tc.draw(gs::integers::<usize>().max_value(2));
    match variant {
        0..=4 | 12 => Action::Burst {
            count: tc.draw(
                gs::integers::<usize>()
                    .min_value(if interleaved { 3 } else { 1 })
                    .max_value(12),
            ),
            unit: choice(),
            host: choice(),
            message: choice(),
            priority: if interleaved {
                Some(6)
            } else {
                let priority = tc.draw(gs::integers::<u8>().max_value(8));
                (priority < 8).then_some(priority)
            },
            step_ms: [0, 1, 2_500, 2_501, 4_999, 5_000]
                [tc.draw(gs::integers::<usize>().max_value(5))],
            interleaved,
            clock: [
                JournalClock::Forward,
                JournalClock::Backward,
                JournalClock::Missing,
            ][tc.draw(gs::integers::<usize>().max_value(2))],
        },
        5 => Action::Advance(
            [
                0, 1, 4_999, 5_000, 14_999, 15_000, 59_999, 60_000, 64_999, 65_000,
            ][tc.draw(gs::integers::<usize>().max_value(9))],
        ),
        6 => Action::Mute(tc.draw(gs::booleans())),
        7 => Action::Solo(tc.draw(gs::booleans()).then(|| UNITS[choice()])),
        8 => Action::Search(QUERIES[tc.draw(gs::integers::<usize>().max_value(4))]),
        9 => Action::Pause(tc.draw(gs::booleans())),
        10 => Action::UnitMute(choice(), tc.draw(gs::booleans())),
        _ => Action::JumpLive,
    }
}

struct Model {
    events: Vec<(u64, Event)>,
    grouping: Grouping,
    row_limit: usize,
    now: u64,
    muted: bool,
    muted_units: [bool; UNITS.len()],
    solo: Option<&'static str>,
    query: &'static str,
    paused: bool,
    anchor: Option<u64>,
}

impl Model {
    fn new(row_limit: usize, grouping: Grouping) -> Self {
        Self {
            events: Vec::new(),
            grouping,
            row_limit,
            now: 0,
            muted: false,
            muted_units: [false; UNITS.len()],
            solo: None,
            query: "",
            paused: false,
            anchor: None,
        }
    }

    fn rows(&self) -> Vec<Vec<usize>> {
        // Replay the whole input. Trace each event back to the last raw event
        // from its source, then follow that event's row assignment.
        let mut rows: Vec<Vec<usize>> = Vec::new();
        let mut assignments: Vec<usize> = Vec::new();
        for (index, (at, event)) in self.events.iter().enumerate() {
            let previous = self.events[..index]
                .iter()
                .rposition(|(_, e)| e.host == event.host && e.unit == event.unit);
            let candidate = previous.and_then(|previous| {
                let last = &self.events[previous].1;
                let row = assignments[previous];
                if row < rows.len().saturating_sub(self.row_limit)
                    || last.priority != event.priority
                    || last.message != event.message
                {
                    return None;
                }
                if previous + 1 == index {
                    return Some(row);
                }
                let first = rows[row][0];
                let (start, first_event) = &self.events[first];
                let journal_span_ok = first_event
                    .realtime_us
                    .zip(event.realtime_us)
                    .is_none_or(|(a, b)| a <= b && b - a <= 5_000_000);
                let journal_order_ok = last
                    .realtime_us
                    .zip(event.realtime_us)
                    .is_none_or(|(a, b)| a <= b);
                (self.grouping == Grouping::Burst
                    && matches!(event.priority, Some(5..=7))
                    && event.unit != "(unknown)"
                    && index - first <= 8
                    && at - start <= 5_000
                    && journal_span_ok
                    && journal_order_ok)
                    .then_some(row)
            });
            let row = candidate.unwrap_or_else(|| {
                rows.push(Vec::new());
                rows.len() - 1
            });
            rows[row].push(index);
            assignments.push(row);
        }
        rows
    }

    fn visible(&self, event: &Event) -> bool {
        let unit_muted = UNITS
            .iter()
            .zip(self.muted_units)
            .any(|(unit, muted)| muted && *unit == event.unit);
        if (self.muted || unit_muted) && matches!(event.priority, Some(5..=7)) {
            return false;
        }
        if self.solo.is_some_and(|unit| unit != event.unit) {
            return false;
        }
        [
            event.unit.as_str(),
            &event.message,
            event.host.as_deref().unwrap_or(""),
        ]
        .into_iter()
        .any(|field| field.to_lowercase().contains(&self.query.to_lowercase()))
    }

    fn visible_ids(&self, rows: &[Vec<usize>]) -> Vec<u64> {
        rows[rows.len().saturating_sub(self.row_limit)..]
            .iter()
            .map(|row| row[0])
            .filter(|index| self.visible(&self.events[*index].1))
            .map(|index| index as u64 + 1)
            .collect()
    }

    fn activity(&self) -> Vec<UnitActivity> {
        let mut by_unit = BTreeMap::<&str, UnitActivity>::new();
        for (at, event) in &self.events {
            let age = self.now - at;
            if age < WINDOW {
                let counts = by_unit.entry(&event.unit).or_insert_with(|| UnitActivity {
                    unit: event.unit.clone(),
                    ..Default::default()
                });
                counts.incoming += 1;
                if self.visible(event) {
                    counts.visible += 1;
                } else {
                    counts.suppressed += 1;
                }
                let bin = 11 - (age / 5_000) as usize;
                counts.sparkline[bin] += 1;
                let severity = match event.priority {
                    Some(0..=3) => BinSeverity::Error,
                    Some(4) => BinSeverity::Warning,
                    _ => BinSeverity::Normal,
                };
                counts.severity[bin] = counts.severity[bin].max(severity);
            }
        }
        let observed_ms = self.now.saturating_sub(5_000).min(WINDOW);
        if observed_ms >= 10_000 {
            for unit in by_unit.values_mut() {
                let baseline = self
                    .events
                    .iter()
                    .filter(|(at, event)| {
                        unit.unit == event.unit && (5_000..65_000).contains(&(self.now - at))
                    })
                    .count() as u64;
                let threshold = (baseline * 10_000).div_ceil(observed_ms).max(3);
                unit.rising = unit.sparkline[11] >= threshold;
            }
        }
        let mut result: Vec<_> = by_unit.into_values().collect();
        result.sort_by_key(|entry| std::cmp::Reverse(entry.incoming));
        result
    }

    fn apply(&mut self, action: &Action, actual: &mut State) {
        match *action {
            Action::Burst {
                count,
                unit,
                host,
                message,
                priority,
                step_ms,
                interleaved,
                clock,
            } => {
                for position in 0..count {
                    let unit = if interleaved {
                        (unit + position % 2) % UNITS.len()
                    } else {
                        unit
                    };
                    self.now += step_ms;
                    let id = self.events.len() as u64 + 1;
                    let event = Event {
                        realtime_us: match clock {
                            JournalClock::Forward => Some(self.now * 1_000),
                            JournalClock::Backward => Some(u64::MAX - id * 1_000),
                            JournalClock::Missing => None,
                        },
                        cursor: Some(id.to_string()),
                        priority,
                        host: HOSTS[host].map(str::to_owned),
                        unit: UNITS[unit].to_owned(),
                        unit_metadata: UnitMetadata::default(),
                        message: MESSAGES[message].to_owned(),
                    };
                    self.events.push((self.now, event.clone()));
                    actual.ingest(event, self.now);
                }
            }
            Action::Advance(ms) => {
                self.now += ms;
                actual.advance(self.now);
            }
            Action::Mute(value) => {
                let incoming: Vec<_> = actual.activity().into_iter().map(|a| a.incoming).collect();
                actual.set_muted(value);
                assert_eq!(
                    incoming,
                    actual
                        .activity()
                        .into_iter()
                        .map(|a| a.incoming)
                        .collect::<Vec<_>>()
                );
                self.muted = value;
            }
            Action::UnitMute(unit, muted) => {
                self.muted_units[unit] = muted;
                actual.set_unit_muted(UNITS[unit], muted).unwrap();
            }
            Action::Solo(unit) => {
                actual.set_solo(unit.map(str::to_owned));
                self.solo = unit;
            }
            Action::Search(query) => {
                actual.set_search(query);
                self.query = query;
            }
            Action::Pause(value) => {
                if value && !self.paused {
                    self.anchor = self.visible_ids(&self.rows()).last().copied();
                } else if !value {
                    self.anchor = None;
                }
                self.paused = value;
                actual.set_paused(value);
            }
            Action::JumpLive => {
                self.paused = false;
                self.anchor = None;
                actual.jump_live();
            }
        }
    }

    fn compare(&self, actual: &State, after: &Action) {
        let rows = self.rows();
        let (evicted, retained) = rows.split_at(rows.len().saturating_sub(self.row_limit));
        assert_eq!(actual.rows().len(), retained.len(), "after {after:?}");
        for (actual, expected) in actual.rows().iter().zip(retained) {
            assert_eq!(actual.count, expected.len() as u64, "after {after:?}");
            assert_eq!(actual.id, expected[0] as u64 + 1);
            assert_eq!(*actual.first, self.events[expected[0]].1);
            assert_eq!(*actual.last, self.events[*expected.last().unwrap()].1);
            let times: Vec<_> = expected[expected.len().saturating_sub(6)..]
                .iter()
                .map(|&index| self.events[index].1.realtime_us)
                .collect();
            assert!(actual.recent_times().iter().copied().eq(times));
        }

        let counters = actual.counters();
        let retained_occurrences = actual.rows().iter().map(|group| group.count).sum::<u64>();
        assert_eq!(counters.received, self.events.len() as u64);
        assert_eq!(counters.evicted_rows, evicted.len() as u64);
        assert_eq!(
            counters.evicted_occurrences + retained_occurrences,
            counters.received
        );
        assert_eq!(counters.activity_dropped, 0);
        assert_eq!(actual.activity(), self.activity(), "after {after:?}");

        let visible_ids = self.visible_ids(&rows);
        assert_eq!(
            actual
                .visible_rows()
                .map(|group| group.id)
                .collect::<Vec<_>>(),
            visible_ids,
            "after {after:?}",
        );
        let selection = if self.paused {
            self.anchor.and_then(|anchor| {
                visible_ids
                    .iter()
                    .rfind(|id| **id <= anchor)
                    .or(visible_ids.first())
            })
        } else {
            visible_ids.last()
        };
        assert_eq!(actual.is_paused(), self.paused);
        assert_eq!(
            actual.selected_group().map(|row| row.id),
            selection.copied(),
            "after {after:?}"
        );
    }
}

#[hegel::test(test_cases = 200)]
fn burst_and_view_actions_match_a_full_history_model(tc: TestCase) {
    let limit = tc.draw(gs::integers::<usize>().min_value(1).max_value(16));
    let actions = tc.draw(gs::vecs(action()).min_size(1).max_size(70));
    for grouping in [Grouping::Burst, Grouping::Adjacent] {
        let mut state = State::new(limit, 1024);
        state.set_grouping(grouping);
        let mut model = Model::new(limit, grouping);
        for action in &actions {
            model.apply(action, &mut state);
            model.compare(&state, action);
        }
    }
}

#[hegel::test(test_cases = 100)]
fn unique_bursts_stay_bounded_without_losing_warnings_when_muted(tc: TestCase) {
    let rows = tc.draw(gs::integers::<usize>().min_value(1).max_value(20));
    let activity = tc.draw(gs::integers::<usize>().min_value(1).max_value(20));
    let extra = tc.draw(gs::integers::<usize>().min_value(1).max_value(200));
    let count = rows.max(activity) + extra;
    let mut state = State::new(rows, activity);
    state.set_muted(true);
    for index in 0..count {
        state.ingest(
            Event {
                realtime_us: Some(index as u64),
                cursor: None,
                priority: Some(4),
                host: None,
                unit: UNITS[0].to_owned(),
                unit_metadata: UnitMetadata::default(),
                message: index.to_string(),
            },
            0,
        );
        assert!(state.rows().len() <= rows);
        assert!(state.activity_len() <= activity);
    }
    assert_eq!(state.visible_rows().count(), rows);
    assert_eq!(state.activity()[0].incoming, activity as u64);
    assert_eq!(state.activity()[0].visible, activity as u64);
    assert_eq!(state.counters().activity_dropped, (count - activity) as u64);
    assert_eq!(state.counters().evicted_occurrences, (count - rows) as u64);
}
