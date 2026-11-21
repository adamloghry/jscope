use jscope::{
    journal::{Decoder, Frame, MAX_LINE_BYTES, UnitField},
    source::{Source, SourceMessage},
    state::{Grouping, State},
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
