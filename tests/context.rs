use hegel::{TestCase, generators as gs};
use jscope::{
    journal::{Decoder, Event, Frame, MAX_LINE_BYTES},
    state::State,
};

fn event(unit: &str, us: Option<u64>, message: &str) -> Event {
    let data = serde_json::to_vec(&serde_json::json!({
        "_SYSTEMD_UNIT": unit, "MESSAGE": message, "PRIORITY": "6",
        "__REALTIME_TIMESTAMP": us.map(|us| us.to_string())
    }))
    .unwrap();
    let Frame::Event(event) = Decoder::new(data.as_slice(), MAX_LINE_BYTES)
        .next_frame()
        .unwrap()
        .unwrap()
    else {
        panic!("expected event")
    };
    event
}

#[hegel::test(test_cases = 100)]
fn context_matches_receipts_in_journal_time_order(tc: TestCase) {
    let anchor = 3_000_000;
    let stamps = [
        None,
        Some(0),
        Some(999_999),
        Some(1_000_000),
        Some(anchor),
        Some(5_000_000),
        Some(5_000_001),
        Some(u64::MAX),
    ];
    let input = tc.draw(gs::vecs(gs::integers::<usize>().max_value(stamps.len() - 1)).max_size(64));
    let mut state = State::default();
    let mut expected = vec![];
    for (index, stamp) in input.into_iter().map(|i| stamps[i]).enumerate() {
        let mut entry = event("db", stamp, "retry");
        entry.cursor = Some(index.to_string());
        if let Some(us) = stamp
            && us.abs_diff(anchor) <= 2_000_000
        {
            expected.push((us, entry.cursor.clone()));
        }
        // Adjacent receipts share the same arrival millisecond, host and message key.
        state.ingest(entry, 0);
    }
    state.set_search("timeout");
    state.ingest(event("api", Some(anchor), "timeout"), 1);
    state.set_paused(true);
    expected.push((anchor, None));
    expected.sort_by_key(|(us, _)| *us);
    state.open_context().unwrap();
    let context = state.context().unwrap();
    let actual: Vec<_> = context
        .events
        .iter()
        .map(|e| (e.realtime_us.unwrap(), e.cursor.clone()))
        .collect();
    assert_eq!(actual, expected);
    assert_eq!(context.events[context.anchor].unit, "api");
    assert_eq!(state.visible_rows().count(), 1);
    state.close_context();
    assert!(state.is_paused());
    assert_eq!(state.selected_group().unwrap().first.message, "timeout");
}

#[test]
fn context_preserves_duplicate_identity_and_snapshot_after_eviction() {
    let mut state = State::new(2, 2);
    for _ in 0..400 {
        state.ingest(event("api", Some(1), "duplicate"), 0);
    }
    assert!(state.begin_spike());
    state.follow_message(0).unwrap();
    state.move_occurrence(270);
    for i in 0..10 {
        state.ingest(event("db", Some(u64::MAX), &format!("later {i}")), 100_000);
    }
    state.open_context().unwrap();
    let view = state.context().unwrap();
    assert_eq!(view.events.len(), 257);
    assert_eq!(view.omitted, 143);
    assert_eq!(view.anchor, 128);
    assert!(std::ptr::eq(
        &*view.events[view.anchor],
        state.spike().unwrap().occurrence().unwrap().event
    ));
    state.move_context(isize::MIN);
    assert_eq!(state.context().unwrap().selected, 0);
    state.move_context(isize::MAX);
    assert_eq!(state.context().unwrap().selected, 256);
    state.ingest(event("api", Some(1), "new entry in old window"), 100_001);
    assert_eq!(state.context().unwrap().events.len(), 257);
    state.close_context();
    assert_eq!(state.spike().unwrap().occurrence().unwrap().index, 270);
}

#[test]
fn missing_journal_time_returns_a_notice_without_entering_context() {
    let mut state = State::default();
    assert!(state.open_context().is_err());
    state.ingest(event("api", None, "unknown time"), 0);
    assert!(state.open_context().unwrap_err().contains("timestamp"));
    assert!(state.context().is_none());
    assert!(!state.is_paused());
}

#[test]
fn expired_intake_still_has_context_from_history_endpoints() {
    let mut state = State::default();
    state.ingest(event("db", Some(1), "wait"), 0);
    state.ingest(event("db", Some(2), "wait"), 1);
    state.ingest(event("api", Some(3), "timeout"), 80_000);
    state.open_context().unwrap();
    assert_eq!(state.context().unwrap().events.len(), 3);
}
