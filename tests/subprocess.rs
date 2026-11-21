use jscope::{
    journal::Frame,
    source::{JournalFilters, Source, SourceMessage},
};
use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

const TIMEOUT: Duration = Duration::from_secs(5);

fn mock(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/helpers")
        .join(name)
}

#[test]
fn journalctl_filters_reach_the_child_and_drop_kills_and_reaps() {
    let filters = JournalFilters {
        backlog: 17,
        units: vec!["demo.service".to_owned(), "other@*.service".to_owned()],
        user_units: vec!["client.service".to_owned()],
        boot: Some("-1".to_owned()),
        priority: Some("warning..alert".to_owned()),
    };
    let source = Source::live_with(mock("journalctl-following").as_os_str(), &filters).unwrap();
    let pid = source.child_id().unwrap();
    let proc_dir = Path::new("/proc").join(pid.to_string());
    assert!(
        proc_dir.exists(),
        "child should remain active in follow mode"
    );
    match source.recv_timeout(TIMEOUT).unwrap() {
        SourceMessage::Frame(Frame::Event(event)) => {
            assert_eq!(event.unit, "demo.service");
            assert_eq!(event.message, "following");
        }
        unexpected => panic!("unexpected subprocess output: {unexpected:?}"),
    }
    // Leave its output queue undrained. Drop must also let a blocked sender exit.
    let start = Instant::now();
    drop(source);
    assert!(start.elapsed() < TIMEOUT);
    assert!(!proc_dir.exists(), "follow child {pid} was not reaped");
}

#[test]
fn child_error_and_bounded_escaped_stderr_reach_the_consumer() {
    let source = Source::live_with(
        mock("journalctl-failing").as_os_str(),
        &JournalFilters::default(),
    )
    .unwrap();
    let pid = source.child_id().unwrap();
    let mut event_seen = false;
    let mut diagnostics = Vec::new();
    loop {
        match source.recv_timeout(TIMEOUT).unwrap() {
            SourceMessage::Frame(Frame::Event(event)) => {
                assert_eq!(event.message, "child output");
                event_seen = true;
            }
            SourceMessage::Diagnostic(text) => diagnostics.push(text),
            SourceMessage::Error(text) => {
                assert!(text.contains("journalctl exited"), "{text}");
                assert!(text.contains('7'), "{text}");
                break;
            }
            unexpected => panic!("unexpected subprocess output: {unexpected:?}"),
        }
    }
    assert!(event_seen);
    assert_eq!(
        diagnostics,
        [
            "bad field: \\x1b[2J",
            "journalctl stderr line exceeded 2048 bytes",
        ],
    );
    assert!(!Path::new("/proc").join(pid.to_string()).exists());
}

#[test]
fn missing_command_fails_at_spawn() {
    let missing = mock("journalctl-does-not-exist");
    let result = Source::live_with(missing.as_os_str(), &JournalFilters::default());
    assert!(matches!(result, Err(err) if err.kind() == std::io::ErrorKind::NotFound));
}
