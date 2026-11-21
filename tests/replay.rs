use jscope::{
    journal::{Decoder, Frame, MAX_LINE_BYTES, UnitField},
    source::{Source, SourceMessage},
    state::{BinSeverity, Grouping, State},
};
use std::{io::BufReader, path::Path, time::Duration};

const DEMO: &str = "fixtures/demo.jsonl";

#[test]
fn synthetic_replay_shows_the_noisy_muted_unit_and_its_warning() {
    let bytes = include_bytes!("../fixtures/demo.jsonl");
    let mut decoder = Decoder::new(BufReader::new(&bytes[..]), MAX_LINE_BYTES);
    let mut state = State::default();
    let mut adjacent = State::default();
    adjacent.set_grouping(Grouping::Adjacent);
    state.set_unit_muted("indexer.service", true).unwrap();
    adjacent.set_unit_muted("indexer.service", true).unwrap();

    let mut at = 0;
    while let Some(frame) = decoder.next_frame().unwrap() {
        let Frame::Event(event) = frame else {
            panic!("demo must contain only journal records");
        };
        adjacent.ingest(event.clone(), at);
        state.ingest(event, at);
        at += 200;
    }
    assert_eq!(at, 2_200);
    assert_eq!(state.counters().received, 11);
    assert!(!state.show_hosts());

    // The proxy warning interrupts the first four polls. The following
    // indexer lease and warning break the indexer's own polling groups.
    let groups = |state: &State| {
        state
            .rows()
            .iter()
            .map(|row| (row.id, row.count))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        groups(&state),
        [(1, 4), (4, 1), (6, 1), (7, 1), (8, 1), (9, 2), (11, 1)]
    );
    assert_eq!(
        groups(&adjacent),
        [
            (1, 3),
            (4, 1),
            (5, 1),
            (6, 1),
            (7, 1),
            (8, 1),
            (9, 2),
            (11, 1)
        ]
    );
    assert_eq!(state.rows()[0].last.cursor.as_deref(), Some("demo-05"));
    assert_eq!(adjacent.rows()[0].last.cursor.as_deref(), Some("demo-03"));
    let activity = state.activity();
    assert_eq!(activity, adjacent.activity());
    assert_eq!(activity.len(), 2);
    assert!(activity.iter().all(|unit| !unit.rising));
    assert!(
        activity
            .iter()
            .all(|unit| unit.severity[11] == BinSeverity::Warning)
    );
    assert_eq!(activity[0].unit, "indexer.service");
    assert_eq!(
        (
            activity[0].incoming,
            activity[0].visible,
            activity[0].suppressed
        ),
        (9, 2, 7)
    );
    assert_eq!(activity[1].unit, "proxy.service");
    assert_eq!(
        (
            activity[1].incoming,
            activity[1].visible,
            activity[1].suppressed
        ),
        (2, 2, 0)
    );

    let rows: Vec<_> = state.visible_rows().collect();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[2].first.message, "Job queue is backing up");
    assert_eq!(rows[2].first.priority, Some(4));
    assert_eq!(rows[2].count, 2);
    assert_eq!(
        rows[2].last.realtime_us.unwrap() - rows[2].first.realtime_us.unwrap(),
        200_000
    );
    assert_eq!(rows[2].first.host.as_deref(), Some("demo"));

    state.toggle_expanded();
    let expanded = state.expanded_group().unwrap();
    assert_eq!(expanded.count, 2);
    assert_eq!(expanded.first.cursor.as_deref(), Some("demo-09"));
    assert_eq!(expanded.last.cursor.as_deref(), Some("demo-10"));

    state.set_unit_muted("indexer.service", false).unwrap();
    assert_eq!(state.activity()[0].incoming, 9);
    assert_eq!(state.activity()[0].visible, 9);
    state.set_unit_muted("indexer.service", true).unwrap();
    state.advance(60_000);
    assert_eq!(state.activity()[0].incoming, 8);
    state.advance(62_000);
    assert!(state.activity().is_empty());
    assert_eq!(state.visible_rows().count(), 3);
}

#[test]
fn lower_ranked_service_signals_a_filtered_error_burst_on_intake_time() {
    fn replay_batch(state: &mut State, at: u64, unit: &str, priority: u8, count: usize) {
        let line = serde_json::json!({
            "_SYSTEMD_UNIT": unit,
            "PRIORITY": priority.to_string(),
            "MESSAGE": "tick",
        });
        let text = format!("{line}\n").repeat(count);
        let mut decoder = Decoder::new(text.as_bytes(), MAX_LINE_BYTES);
        while let Some(frame) = decoder.next_frame().unwrap() {
            let Frame::Event(event) = frame else {
                panic!("expected a journal entry");
            };
            state.ingest(event, at);
        }
    }

    let mut state = State::default();
    state.set_search("steady");
    state.set_unit_muted("worker.service", true).unwrap();
    for second in 0..=65 {
        let at = second * 1_000;
        replay_batch(&mut state, at, "steady.service", 6, 1);
        match second {
            1 => replay_batch(&mut state, at, "worker.service", 6, 1),
            25 => replay_batch(&mut state, at, "worker.service", 6, 17),
            59 => replay_batch(&mut state, at, "worker.service", 4, 1),
            64 => {
                for priority in [4, 3, 6] {
                    replay_batch(&mut state, at, "worker.service", priority, 1);
                }
            }
            _ => {}
        }
    }

    // The oldest worker arrival is outside the graph but inside the rate comparison.
    // Three arrivals in five seconds fall just short of doubling 19 per minute.
    state.advance(65_999);
    let before = state.activity();
    assert_eq!(before[1].incoming, 21);
    assert!(!before[1].rising);

    // Once that arrival expires, three in five seconds is exactly double 18/min.
    state.advance(66_000);
    let after = state.activity();
    assert_eq!(after[0].unit, "steady.service");
    assert!(!after[0].rising);
    let worker = &after[1];
    assert_eq!(worker.unit, "worker.service");
    assert!(worker.rising);
    assert_eq!(
        (worker.incoming, worker.visible, worker.suppressed),
        (21, 0, 21)
    );
    assert_eq!(&worker.sparkline[10..], &[1, 3]);
    assert_eq!(
        &worker.severity[10..],
        &[BinSeverity::Warning, BinSeverity::Error]
    );

    state.advance(69_000);
    assert!(!state.activity()[1].rising);
    state.advance(124_000);
    assert!(
        state
            .activity()
            .iter()
            .all(|unit| unit.unit != "worker.service")
    );
}

#[test]
fn spike_replay_lands_on_the_occurrence_inside_the_frozen_interval() {
    let fixture = include_bytes!("../fixtures/demo.jsonl");
    let mut decoder = Decoder::new(fixture.as_slice(), MAX_LINE_BYTES);
    let mut state = State::default();
    for at in [
        0, 0, 0, 3_000, 5_000, 5_002, 7_000, 7_500, 10_000, 10_001, 13_000,
    ] {
        let Some(Frame::Event(event)) = decoder.next_frame().unwrap() else {
            panic!("missing demo event");
        };
        state.ingest(event, at);
    }
    state.advance(15_000);
    state.set_search("proxy");
    state.set_solo(Some("proxy.service".into()));
    state.set_unit_muted("indexer.service", true).unwrap();
    let activity = state.activity();
    assert_eq!(activity[0].visible, 0);
    assert_eq!(state.visible_rows().count(), 2);

    assert!(state.begin_spike());
    assert_eq!(state.spike().unwrap().selected, 11);
    state.jump_to_spike().unwrap();
    let group = state.selected_group().unwrap();
    assert_eq!(group.first.cursor.as_deref(), Some("demo-09"));
    assert_eq!(group.count, 2);
    // Just one millisecond apart on intake, the adjacent warnings straddle the bin edge.
    let hit = state.spike_hit().unwrap();
    assert_eq!(hit.cursor.as_deref(), Some("demo-10"));
    assert_eq!(hit.realtime_us, Some(1_760_000_001_800_000));
    assert!(state.spike_hit_hidden());
    assert_eq!(state.visible_rows().count(), 3);
    assert_eq!(state.activity(), activity);
    assert_eq!(state.search(), "proxy");
    assert_eq!(state.solo(), Some("proxy.service"));
    assert!(state.is_unit_muted("indexer.service"));
    state.toggle_expanded();
    assert_eq!(state.expanded_group().unwrap().id, 9);
    assert!(state.spike_hit().is_some());
    state.move_row(-1);
    assert!(state.spike_hit().is_none());
    assert_eq!(state.selected_group().unwrap().id, 8);
    assert_eq!(state.visible_rows().count(), 2);

    state.jump_live();
    assert!(state.begin_spike());
    state.move_spike(-2);
    assert_eq!(state.spike().unwrap().bins[9].count, 1);
    state.advance(90_000);
    assert!(state.activity().is_empty());
    state.jump_to_spike().unwrap();
    let poll = state.selected_group().unwrap();
    assert_eq!((poll.id, poll.count), (1, 4));
    assert_eq!(poll.first.cursor.as_deref(), Some("demo-01"));
    assert_eq!(
        state.spike_hit().unwrap().cursor.as_deref(),
        Some("demo-05")
    );
    state.jump_live();
    assert!(state.spike_hit().is_none());
    assert_eq!(state.visible_rows().count(), 2);
}

#[test]
fn evicted_spike_target_keeps_the_snapshot_and_never_substitutes_a_later_error() {
    let mut state = State::new(2, 20);
    receive(&mut state, 0, "worker.service", 3, "error", "original");
    assert!(state.begin_spike());
    receive(&mut state, 6_000, "worker.service", 3, "different", "next");
    receive(&mut state, 6_001, "worker.service", 3, "error", "later");
    assert_eq!(state.counters().evicted_rows, 1);
    assert!(state.jump_to_spike().unwrap_err().contains("evicted"));
    assert_eq!(state.spike().unwrap().bins[11].count, 1);
    assert!(state.spike_hit().is_none());
    state.move_spike(isize::MIN);
    assert!(state.jump_to_spike().unwrap_err().contains("empty"));
    state.move_spike(isize::MAX);
    assert_eq!(state.spike().unwrap().selected, 11);
    state.cancel_spike();
    assert!(!state.is_paused());
    assert!(state.begin_spike());
    state.jump_to_spike().unwrap();
    assert_eq!(state.spike_hit().unwrap().cursor.as_deref(), Some("next"));
}

fn receive(state: &mut State, at: u64, unit: &str, priority: u8, message: &str, cursor: &str) {
    let record = serde_json::json!({
        "_SYSTEMD_UNIT": unit, "PRIORITY": priority.to_string(),
        "MESSAGE": message, "__CURSOR": cursor,
    });
    let line = record.to_string();
    let Some(Frame::Event(event)) = Decoder::new(line.as_bytes(), MAX_LINE_BYTES)
        .next_frame()
        .unwrap()
    else {
        panic!("expected synthetic journal entry");
    };
    state.ingest(event, at);
}

#[test]
fn demo_compares_the_muted_volume_driver_with_the_peer_warning() {
    let mut state = State::default();
    let mut decoder = Decoder::new(
        include_bytes!("../fixtures/demo.jsonl").as_slice(),
        MAX_LINE_BYTES,
    );
    while let Some(Frame::Event(event)) = decoder.next_frame().unwrap() {
        state.ingest(event, 0);
    }
    state.set_unit_muted("indexer.service", true).unwrap();
    state.toggle_pin().unwrap();
    assert!(state.is_pinned("indexer.service"));
    state.move_unit(1);
    assert!(state.begin_spike());
    let snapshot = state.spike().unwrap();
    assert_eq!(snapshot.unit, "proxy.service");
    let peer = snapshot.peer.as_ref().unwrap();
    assert_eq!(peer.unit, "indexer.service");
    let bin = &peer.bins[11];
    assert_eq!(bin.count, 9);
    assert_eq!(bin.event().unwrap().cursor.as_deref(), Some("demo-09"));
    let drivers: Vec<(&str, u64)> = bin
        .drivers
        .iter()
        .map(|d| (d.message.as_ref(), d.count))
        .collect();
    assert_eq!(
        drivers,
        [
            ("Polling job queue", 6),
            ("Job queue is backing up", 2),
            ("Leased work item 82", 1)
        ]
    );
    // Subsequent arrivals and even expiry of both live graphs leave the paired snapshot intact.
    receive(&mut state, 60_000, "proxy.service", 0, "new error", "new");
    state.advance(130_000);
    state.move_spike(-1);
    state.switch_spike_unit();
    assert_eq!(state.spike().unwrap().selected, 10);
    state.move_spike(1);
    assert_eq!(state.spike().unwrap().bins[11].count, 9);
    state.switch_spike_unit();
    assert_eq!(state.spike().unwrap().bins[11].count, 2);
    state.jump_to_spike().unwrap();
    assert_eq!(
        state.spike_hit().unwrap().cursor.as_deref(),
        Some("demo-04")
    );
}

#[test]
fn replacing_a_pin_requires_unpinning_and_quiet_pins_stay_reachable() {
    let mut state = State::default();
    assert!(state.toggle_pin().is_err());
    for unit in ["a", "b", "c"] {
        receive(&mut state, 0, unit, 6, "routine", unit);
    }
    state.toggle_pin().unwrap();
    // A single pin inspected on its own never duplicates its graph.
    assert!(state.begin_spike());
    assert!(state.spike().unwrap().peer.is_none());
    state.cancel_spike();
    state.move_unit(1);
    state.toggle_pin().unwrap();
    state.move_unit(1);
    assert!(state.toggle_pin().is_err());
    assert!(state.is_pinned("a") && state.is_pinned("b") && !state.is_pinned("c"));
    // An unrelated selection does not silently displace a member of the pair.
    assert!(state.begin_spike());
    assert!(state.spike().unwrap().peer.is_none());
    state.cancel_spike();
    state.advance(60_000);
    let sidebar = state.sidebar_activity();
    assert_eq!(
        sidebar
            .iter()
            .map(|entry| (entry.unit.as_str(), entry.incoming))
            .collect::<Vec<_>>(),
        [("a", 0), ("b", 0)]
    );
    state.move_unit(isize::MAX);
    state.toggle_pin().unwrap();
    assert!(!state.is_pinned("b"));
    receive(&mut state, 60_001, "c", 4, "return", "new");
    state.move_unit(isize::MIN);
    state.toggle_pin().unwrap();
    assert!(state.is_pinned("a") && state.is_pinned("c"));
    assert!(state.begin_spike());
    assert_eq!(state.spike().unwrap().unit, "c");
    assert_eq!(state.spike().unwrap().peer.as_ref().unwrap().unit, "a");
}

#[test]
fn service_hops_keep_the_selected_lane_through_filtered_entries_and_expiry() {
    let mut state = State::default();
    receive(&mut state, 0, "api", 4, "old warning", "api-1");
    receive(&mut state, 5_000, "db", 3, "peer error", "db-1");
    receive(&mut state, 10_000, "api", 6, "routine", "api-2");
    receive(&mut state, 15_000, "api", 3, "failed", "api-3");
    receive(&mut state, 15_000, "api", 0, "down", "api-4");
    state.set_solo(Some("db".into()));
    state.toggle_pin().unwrap(); // api
    state.move_unit(1);
    state.toggle_pin().unwrap(); // db
    assert!(state.begin_spike());
    state.switch_spike_unit();
    state.move_spike(isize::MAX);
    state.jump_to_spike().unwrap();
    assert_eq!(state.spike_hit().unwrap().cursor.as_deref(), Some("api-3"));
    state.toggle_expanded();
    receive(&mut state, 90_000, "db", 0, "unrelated later spike", "live");
    state.hop_spike(-1).unwrap();
    assert_eq!(state.spike_hit().unwrap().cursor.as_deref(), Some("api-1"));
    assert!(state.spike_hit_hidden());
    assert!(state.expanded_group().is_none());
    assert!(state.hop_spike(-1).is_err());
    state.hop_spike(1).unwrap();
    assert_eq!(state.spike_hit().unwrap().cursor.as_deref(), Some("api-3"));
    assert!(state.hop_spike(1).is_err());
    assert_eq!(state.solo(), Some("db"));
    state.jump_live();
    state.hop_spike(-1).unwrap();
    assert_eq!(state.spike_hit().unwrap().cursor.as_deref(), Some("live"));
}

#[test]
fn driver_cutoff_breaks_ties_by_exact_text_even_when_the_jump_target_falls_below_it() {
    let mut state = State::default();
    for (priority, text) in [
        (3, "timeout"),
        (6, "retry"),
        (6, "retrying"),
        (6, "Retry"),
        (6, "poll"),
    ] {
        receive(&mut state, 0, "api", priority, text, text);
    }
    assert!(state.begin_spike());
    let bin = &state.spike().unwrap().bins[11];
    assert_eq!(bin.count, 5);
    let drivers: Vec<(&str, u64)> = bin
        .drivers
        .iter()
        .map(|d| (d.message.as_ref(), d.count))
        .collect();
    assert_eq!(drivers, [("Retry", 1), ("poll", 1), ("retry", 1)]);
    state.jump_to_spike().unwrap();
    assert_eq!(state.spike_hit().unwrap().message, "timeout");
}

#[test]
fn drivers_and_hops_obey_the_independent_activity_and_row_limits() {
    let mut state = State::new(4, 4);
    for _ in 0..5 {
        receive(&mut state, 0, "api", 4, "over budget", "dropped");
    }
    for (at, level, message, cursor) in [
        (10_000, 4, "slow", "older"),
        (15_000, 6, "ping", "a"),
        (15_000, 6, "ping", "b"),
        (15_000, 4, "ping", "first"),
        (15_000, 3, "retry", "error"),
    ] {
        receive(&mut state, at, "api", level, message, cursor);
    }
    assert_eq!(state.counters().activity_dropped, 5);
    assert!(state.begin_spike());
    let bin = &state.spike().unwrap().bins[11];
    let drivers: Vec<(&str, u64)> = bin
        .drivers
        .iter()
        .map(|d| (d.message.as_ref(), d.count))
        .collect();
    assert_eq!(drivers, [("ping", 3), ("retry", 1)]);
    assert_eq!(bin.count, 4);
    assert_eq!(
        state
            .spike()
            .unwrap()
            .bins
            .iter()
            .map(|b| b.count)
            .sum::<u64>(),
        5
    );
    state.jump_to_spike().unwrap();
    // The exact older warning originally existed. A new copy evicts its row.
    receive(&mut state, 70_000, "api", 4, "slow", "lookalike");
    assert!(state.hop_spike(-1).unwrap_err().contains("evicted"));
    assert_eq!(state.spike_hit().unwrap().cursor.as_deref(), Some("error"));
    state.jump_live();
    state.hop_spike(-1).unwrap();
    assert_eq!(
        state.spike_hit().unwrap().cursor.as_deref(),
        Some("lookalike")
    );
}

#[test]
fn replay_file_source_preserves_order_and_reports_eof() {
    let source = Source::input(&Path::new(env!("CARGO_MANIFEST_DIR")).join(DEMO)).unwrap();
    assert_eq!(source.child_id(), None);
    for number in 1..=11 {
        let SourceMessage::Frame(Frame::Event(event)) =
            source.recv_timeout(Duration::from_secs(5)).unwrap()
        else {
            panic!("missing demo record {number}");
        };
        assert_eq!(event.cursor, Some(format!("demo-{number:02}")));
    }
    assert_eq!(
        source.recv_timeout(Duration::from_secs(5)).unwrap(),
        SourceMessage::Finished,
    );
}

#[test]
fn parser_regression_fixture_covers_duplicate_binary_and_control_fields() {
    let bytes = include_bytes!("../fixtures/parser-regressions.jsonl");
    let mut decoder = Decoder::new(BufReader::with_capacity(5, &bytes[..]), MAX_LINE_BYTES);
    let Some(Frame::Event(first)) = decoder.next_frame().unwrap() else {
        panic!("fixture must start with an event");
    };
    assert_eq!(first.realtime_us, Some(1234));
    assert_eq!(first.cursor.as_deref(), Some("first"));
    assert_eq!(first.host.as_deref(), Some("demo"));
    assert_eq!(first.unit, "client.service");
    assert_eq!(
        first.unit_fields().collect::<Vec<_>>(),
        [
            (UnitField::UserUnit, "client.service"),
            (UnitField::SystemUnit, "session.scope")
        ]
    );
    assert_eq!(first.priority, Some(4));
    assert_eq!(first.message, "\\x1b[2J\\xff");
    assert_eq!(decoder.next_frame().unwrap(), Some(Frame::Malformed));

    let Some(Frame::Event(next)) = decoder.next_frame().unwrap() else {
        panic!("decoder must recover after malformed input");
    };
    assert_eq!(next.unit, "legacy");
    assert_eq!(
        next.unit_fields().collect::<Vec<_>>(),
        [
            (UnitField::SyslogIdentifier, "legacy"),
            (UnitField::Command, "fallback")
        ]
    );
    assert_eq!(next.message, "line\\nbackslash \\\\ and U+009B \\u{9b}");

    let Some(Frame::Event(command)) = decoder.next_frame().unwrap() else {
        panic!("fixture must cover the command fallback");
    };
    assert_eq!(command.unit, "tail");
    assert_eq!(
        command.unit_fields().collect::<Vec<_>>(),
        [(UnitField::Command, "tail")]
    );
    assert_eq!(command.unit_metadata.heap_bytes(), 0);
    assert_eq!(command.priority, None);

    let Some(Frame::Event(last)) = decoder.next_frame().unwrap() else {
        panic!("fixture must end with a missing message");
    };
    assert_eq!(last.unit, "(unknown)");
    assert!(last.unit_fields().next().is_none());
    assert_eq!(last.message, "\\(message unavailable)");
    assert_eq!(decoder.next_frame().unwrap(), None);
}
