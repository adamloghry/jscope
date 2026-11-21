use jscope::{
    journal::{Decoder, Event, Frame, MAX_LINE_BYTES},
    state::State,
};
use serde_json::json;

const PAYLOAD_BYTES: usize = 60 * 1024;
const TEXT_BUDGET: usize = 12 * 1024 * 1024;

fn event(unit: &str, host: &str, priority: u8, message: &str, timestamp: usize) -> Event {
    let line = json!({
        "_SYSTEMD_UNIT": unit,
        "_HOSTNAME": host,
        "PRIORITY": priority.to_string(),
        "MESSAGE": message,
        "__REALTIME_TIMESTAMP": timestamp.to_string(),
        "__CURSOR": format!("cursor-{timestamp}"),
    })
    .to_string();
    let Some(Frame::Event(event)) = Decoder::new(line.as_bytes(), MAX_LINE_BYTES)
        .next_frame()
        .unwrap()
    else {
        panic!("test input must fit the production journal line limit");
    };
    event
}

#[test]
fn large_distinct_messages_hit_byte_budgets_before_entry_caps() {
    let mut state = State::default();
    let payload = "a".repeat(PAYLOAD_BYTES);
    let count = TEXT_BUDGET / PAYLOAD_BYTES + 15;
    for index in 0..count {
        let message = format!("{index} {payload}");
        state.ingest(event("test.service", "fixture", 4, &message, index), 100);
    }

    assert!(state.rows().len() <= TEXT_BUDGET / PAYLOAD_BYTES);
    assert!(state.activity_len() <= TEXT_BUDGET / PAYLOAD_BYTES);
    let counters = state.counters();
    assert_eq!(counters.received, count as u64);
    assert_eq!(
        counters.evicted_occurrences,
        (count - state.rows().len()) as u64
    );
    assert_eq!(
        counters.activity_dropped,
        (count - state.activity_len()) as u64
    );
    assert_eq!(state.activity()[0].incoming, state.activity_len() as u64);
    assert_eq!(state.visible_rows().count(), state.rows().len());
    assert_eq!(
        state.rows().back().unwrap().last.realtime_us,
        Some(count as u64 - 1)
    );
}

#[test]
fn large_repeat_groups_share_intake_and_keep_occurrence_counts_after_tail_eviction() {
    let mut state = State::default();
    let payload = "b".repeat(PAYLOAD_BYTES);
    let groups = TEXT_BUDGET / (2 * PAYLOAD_BYTES) + 15;
    for index in 0..groups {
        let message = format!("{index} {payload}");
        state.ingest(
            event("test.service", "fixture", 4, &message, index * 2),
            100,
        );
        state.ingest(
            event("test.service", "fixture", 4, &message, index * 2 + 1),
            100,
        );
    }

    assert!(state.rows().len() <= TEXT_BUDGET / (2 * PAYLOAD_BYTES));
    assert!(state.rows().iter().all(|group| group.count == 2));
    let counters = state.counters();
    assert_eq!(counters.received, (groups * 2) as u64);
    assert_eq!(
        counters.evicted_occurrences,
        (2 * (groups - state.rows().len())) as u64
    );
    assert_eq!(counters.activity_dropped, 0);
    assert_eq!(state.activity_len(), groups);
    assert_eq!(state.activity()[0].incoming, counters.received);
    let latest = state.rows().back().unwrap();
    assert_eq!(latest.first.realtime_us, Some((groups * 2 - 2) as u64));
    assert_eq!(latest.last.realtime_us, Some((groups * 2 - 1) as u64));
}

#[test]
fn repeat_boundaries_include_hidden_entries_hosts_units_levels_and_messages() {
    let mut state = State::default();
    state.set_muted(true);
    let records = [
        ("one", "local", 4, "same"),
        ("one", "local", 4, "same"),
        ("one", "local", 6, "same"),
        ("one", "local", 4, "same"),
        ("one", "remote", 4, "same"),
        ("two", "remote", 4, "same"),
        ("two", "remote", 4, "same!"),
        ("two", "remote", 4, "same!"),
    ];
    for (index, (unit, host, priority, message)) in records.into_iter().enumerate() {
        state.ingest(event(unit, host, priority, message, index), 100);
    }

    assert_eq!(
        state
            .rows()
            .iter()
            .map(|group| group.count)
            .collect::<Vec<_>>(),
        [2, 1, 1, 1, 1, 2]
    );
    assert_eq!(
        state
            .visible_rows()
            .map(|group| group.count)
            .collect::<Vec<_>>(),
        [2, 1, 1, 1, 2]
    );
    assert_eq!(
        state
            .activity()
            .iter()
            .map(|entry| entry.incoming)
            .sum::<u64>(),
        8
    );
    assert_eq!(
        state
            .activity()
            .iter()
            .map(|entry| entry.visible)
            .sum::<u64>(),
        7
    );
    assert!(state.show_hosts());
}
