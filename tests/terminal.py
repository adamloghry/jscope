"""Drive the packaged live UI and piped replay through a controlling terminal."""

from contextlib import contextmanager
import fcntl
import json
import os
from pathlib import Path
import pty
import select
import subprocess
import sys
from tempfile import TemporaryDirectory
import termios
import time

import pyte


@contextmanager
def terminal(command, *, columns, lines, piped=False, env=None):
    master, slave = pty.openpty()
    try:
        screen = pyte.Screen(columns=columns, lines=lines)
        stream = pyte.ByteStream(screen)
        termios.tcsetwinsize(slave, (lines, columns))
        initial_settings = termios.tcgetattr(slave)

        def claim_terminal():
            os.setsid()
            fcntl.ioctl(slave, termios.TIOCSCTTY, 0)

        with subprocess.Popen(
            command,
            stdin=subprocess.PIPE if piped else slave,
            stdout=slave,
            stderr=slave,
            pass_fds=(slave,),
            preexec_fn=claim_terminal,
            env={**os.environ, "TERM": "xterm", **(env or {})},
        ) as scope:
            terminal_output = bytearray()

            def drain(timeout):
                if select.select([master], [], [], timeout)[0]:
                    data = os.read(master, 65536)
                    stream.feed(data)
                    terminal_output.extend(data)

            def wait_for(*expected, absent=()):
                deadline = time.monotonic() + 10
                while time.monotonic() < deadline and scope.poll() is None:
                    contents = "\n".join(screen.display)
                    if all(text in contents for text in expected) and not any(
                        text in contents for text in absent
                    ):
                        return contents
                    drain(0.1)
                raise AssertionError(
                    f"Waiting for {expected} with {absent} absent; exit status {scope.poll()}\n"
                    + "\n".join(screen.display)
                )

            def keys(data):
                os.write(master, data)

            try:
                yield scope, keys, wait_for
                keys(b"q")
                assert scope.wait(timeout=10) == 0
                while select.select([master], [], [], 0)[0]:
                    drain(0)
                assert b"\x1b[?1049l" in terminal_output
                assert termios.tcgetattr(slave) == initial_settings
            finally:
                if scope.poll() is None:
                    scope.terminate()
                    try:
                        scope.wait(timeout=2)
                    except subprocess.TimeoutExpired:
                        scope.kill()
    finally:
        os.close(master)
        os.close(slave)


def journal_record(unit, level, message, realtime_us=0, cursor=None):
    record = {
        "_SYSTEMD_UNIT": unit,
        "PRIORITY": str(level),
        "_HOSTNAME": "fixture",
        "MESSAGE": message,
        "__REALTIME_TIMESTAMP": str(realtime_us),
    }
    if cursor is not None:
        record["__CURSOR"] = cursor
    return json.dumps(record) + "\n"


def test_default_live(binary):
    with TemporaryDirectory() as directory:
        # Only this fixture is on PATH. Keep stdout open as journalctl --follow would.
        journalctl = Path(directory) / "journalctl"
        backlog = "".join(
            journal_record(
                "worker.service", 4 if i == 31 else 6, f"entry-{i:02}", i * 1_000_000
            )
            for i in range(32)
        )
        journalctl.write_text(
            f"#!{sys.executable}\n"
            "import sys, signal\n"
            f"sys.stdout.write({backlog!r})\n"
            "sys.stdout.flush()\n"
            "signal.pause()\n",
            encoding="utf-8",
        )
        journalctl.chmod(0o755)
        env = {"PATH": directory, "TZ": "JST-9"}
        with terminal([binary], columns=80, lines=24, env=env) as (_, keys, wait_for):
            contents = wait_for("32/min", "100% shown", "entry-31")
            header, *body = contents.splitlines()
            expected = ("JournalScope", "Live", "journalctl", "32/min", "100% shown")
            assert all(text in header for text in expected), contents
            assert "▰" in header and "▱" not in header, contents
            event_rows = [row for row in body if "entry-" in row]
            assert len(event_rows) >= 20, contents
            assert "INFO" in event_rows[-2] and "entry-30" in event_rows[-2], contents
            assert "09:00:31" in event_rows[-1] and "WARN" in event_rows[-1], contents
            diagnostics = ("bad json", "malformed", "oversize", "evicted", "omitted")
            assert not any(text in contents.lower() for text in diagnostics), contents

            keys(b"m")
            muted = wait_for("32/min", "3% shown", "entry-31", absent=("entry-30",))
            assert "▱" in muted.splitlines()[0], muted
            keys(b"m")
            wait_for("100% shown", "entry-30")
            keys(b"\t")
            wait_for("worker.service", "32/32/0", absent=("entry-30",))
            keys(b"\t")
            wait_for("entry-30", "entry-31")


def test_replay(binary):
    command = [binary, "--input", "-", "--no-color", "--mute-unit", "loud.service"]
    with terminal(command, columns=140, lines=30, piped=True) as (scope, keys, wait_for):
        def send(unit, level, message):
            scope.stdin.write(journal_record(unit, level, message).encode())
            scope.stdin.flush()

        wait_for("JournalScope", "replay", "Waiting for journal entries")
        send("loud.service", 4, "ANCHOR_WATCH")
        send("quiet.service", 6, "QUIET_INFO_WATCH")
        send("loud.service", 6, "LOUD_HIDDEN")
        contents = wait_for("ANCHOR_WATCH", "QUIET_INFO_WATCH", "2/1/1", "3/min")
        assert "LOUD_HIDDEN" not in contents
        loud = next(row for row in contents.splitlines() if "2/1/1" in row)
        assert "!" in loud and "↑ loud.service" not in loud, contents

        keys(b" ")
        wait_for("Paused")
        send("loud.service", 4, "LATER_WATCH")
        send("loud.service", 6, "LOUD_MORE_HIDDEN")
        contents = wait_for("Paused", "5/min", "4/2/2", "ANCHOR_WATCH", "QUIET_INFO_WATCH")
        assert "LATER_WATCH" not in contents

        keys(b"/watch\r")
        wait_for("/ watch")
        send("loud.service", 4, "WATCH_SEARCH_NEW")
        send("quiet.service", 6, "QUIET_FILTERED")
        contents = wait_for("Paused", "7/min", "5/3/2", "2/1/1", "ANCHOR_WATCH")
        assert "WATCH_SEARCH_NEW" not in contents
        assert "QUIET_FILTERED" not in contents

        keys(b"g")
        contents = wait_for("Live", "LATER_WATCH", "WATCH_SEARCH_NEW", "QUIET_INFO_WATCH")
        assert "QUIET_FILTERED" not in contents
        keys(b"/\x15\r")
        wait_for("QUIET_FILTERED")

        keys(b"\tM")
        wait_for("LOUD_HIDDEN", "LOUD_MORE_HIDDEN", "5/5/0", "QUIET_FILTERED")
        keys(b"M")
        contents = wait_for("5/3/2", "QUIET_FILTERED")
        assert "LOUD_HIDDEN" not in contents
        assert "LOUD_MORE_HIDDEN" not in contents
        scope.stdin.write(b"this is not JSON\n")
        scope.stdin.flush()
        wait_for("1 malformed", "7/min", "QUIET_FILTERED")

        # The graphs keep the highest incoming severity even when search hides that row.
        keys(b"/quiet\r")
        send("loud.service", 3, "LOUD_ERROR_UNMATCHED")
        contents = wait_for("8/min", "6/0/6", absent=("LOUD_ERROR_UNMATCHED",))
        loud = next(row for row in contents.splitlines() if "6/0/6" in row)
        assert "×" in loud and "×" in contents.splitlines()[0], contents
        # Leave stdin open so quitting must interrupt the waiting reader.


def test_grouped_demo(binary, fixture):
    for options, repeat, polling_rows, latest_cursor in (
        ([], "×4/800ms", 3, "demo-05"),
        (["--adjacent-only"], "×3/400ms", 4, "demo-03"),
    ):
        command = [binary, "--input", fixture, "--no-color", *options]
        with terminal(command, columns=80, lines=24) as (_, keys, wait_for):
            contents = wait_for("replay complete", "11/min", "9/9/0", repeat)
            rows = [line for line in contents.splitlines() if "Polling job queue" in line]
            assert len(rows) == polling_rows, contents
            assert repeat in rows[0], contents
            assert all("indexer" in row.split() and ".service" not in row for row in rows), contents
            assert "proxy WARN Backend latency above target" in contents, contents
            assert "indexer.service" in contents, contents  # Full name in the activity strip.
            leader = next(line for line in contents.splitlines() if "Noisiest" in line)
            assert "!" in leader and "↑ indexer.service" not in leader, contents
            keys(b"\x1b[A" * 10 + b"\r")
            wait_for("_SYSTEMD_UNIT=indexer.service", f"Latest cursor {latest_cursor}")


def test_jump_to_spike(binary):
    command = [binary, "--input", "-", "--no-color"]
    with terminal(command, columns=80, lines=24, piped=True, env={"TZ": "UTC0"}) as (scope, keys, wait_for):
        records = [
            ("worker.service", 4, "early warning", 1_000_000, "warning"),
            ("worker.service", 3, "connection refused", 2_000_000, "first-error"),
            ("worker.service", 3, "connection refused", 3_000_000, "repeated-error"),
            ("worker.service", 0, "later emergency", 4_000_000, "emergency"),
            ("other.service", 6, "keep this visible", 5_000_000, "heartbeat"),
        ]
        scope.stdin.write("".join(journal_record(*record) for record in records).encode())
        scope.stdin.flush()
        wait_for("5/min", "connection refused")
        keys(b"/keep this visible\r\t")
        wait_for("worker.service", "4/0/4", "Enter spike")
        keys(b"\r")
        wait_for("Spike · 60s snapshot", "[×]", "connection refused", "Enter jump")
        keys(b"\x1b")
        wait_for("Enter spike", absent=("60s snapshot",))
        keys(b"\r")
        wait_for("0–5s before capture", "4 entries")

        # New arrivals change the live graph while the chosen bar keeps its original target.
        scope.stdin.write(journal_record("worker.service", 3, "new arrival", 6_000_000, "new-error").encode())
        scope.stdin.flush()
        wait_for("6/min", "4 entries")
        keys(b"\x1b[D\r")
        wait_for("5–10s before capture", "This interval is empty")
        keys(b"\x1b[C\r")
        contents = wait_for("Paused", "outside filters", "00:00:02", "connection refused")
        assert "later emergency" not in contents, contents
        keys(b"\r")
        wait_for("Spike cursor first-error", "Latest cursor repeated-error")
        keys(b"g")
        wait_for("Live", "17% shown", "keep this visible", absent=("outside filters", "connection refused"))


def test_compare_spike_drivers(binary, fixture):
    command = [binary, "--input", fixture, "--mute-unit", "indexer.service", "--no-color"]
    for columns in (80, 104):
        with terminal(command, columns=columns, lines=24, env={"TZ": "UTC0"}) as (_, keys, wait_for):
            wait_for("11/min", "9/2/7")
            keys(b"\tp\x1b[Bp\r")
            contents = wait_for("Top messages", "↑↓ service", "› proxy.service", "1/2 · Backend latency")
            assert any("proxy.service" in row and "08:53:20.600" in row for row in contents.splitlines()), contents
            assert any("indexer.service" in row and "08:53:21.600" in row for row in contents.splitlines()), contents
            keys(b"\x1b[B")
            contents = wait_for("› indexer.service", "?→6/9 · Polling job queue", "?→2/9 · Job queue")
            assert "1/9 · Leased work item" in contents, contents
            assert contents.count("[!") == 2, contents
            keys(b"\x1b[D")
            contents = wait_for("5–10s before capture", "empty interval", absent=("6/9 ·",))
            assert contents.count("[ ·]") == 2, contents
            keys(b"\x1b[C\r\r")
            wait_for("Spike cursor demo-09", "Latest cursor demo-10")


def test_tail_spike_hops(binary):
    with terminal([binary, "--input", "-", "--no-color"], columns=80, lines=24, piped=True) as (scope, keys, wait_for):
        def send(unit, priority, message, cursor):
            scope.stdin.write(journal_record(unit, priority, message, cursor=cursor).encode())
            scope.stdin.flush()

        send("db.service", 4, "lock wait", "old-warning")
        wait_for("1/min", "lock wait")
        # Journal timestamps are identical; only delivery time separates the graph bins.
        time.sleep(5.2)
        send("api.service", 3, "db unavailable", "new-error")
        send("health.service", 6, "heartbeat", "healthy")
        wait_for("3/min", "heartbeat")
        keys(b"/heartbeat\r[\r")
        wait_for("outside filters", "Spike cursor new-error")
        # A later error in this same bin must not replace the captured destination.
        send("api.service", 2, "another failure", "later")
        wait_for("4/min", "Spike cursor new-error")
        keys(b"[\r")
        wait_for("outside filters", "Spike cursor old-warning")
        keys(b"[")
        wait_for("No older spike", "Spike cursor old-warning")
        keys(b"]\r")
        wait_for("outside filters", "Spike cursor new-error")
        keys(b"]")
        wait_for("No newer spike", "Spike cursor new-error")
        keys(b"g")
        wait_for("Live", "25% shown", "heartbeat", absent=("outside filters", "Spike cursor"))


def test_follow_ranked_message(binary, fixture):
    command = [binary, "--input", fixture, "--mute-unit", "indexer.service", "--no-color"]
    with terminal(command, columns=80, lines=24, env={"TZ": "UTC0"}) as (_, keys, wait_for):
        wait_for("11/min", "9/2/7")
        keys(b"\t\r1")
        wait_for("Message occurrences", "Occurrence 1/6", "08:53:20.000", "Cursor demo-01")
        for ordinal, cursor in enumerate(("02", "03", "05", "07", "11"), 2):
            keys(b"\x1b[C")
            wait_for(f"Occurrence {ordinal}/6", f"Cursor demo-{cursor}", "Polling job queue")
        keys(b"\x1b")
        wait_for("Top messages", "1–3 follow", "[1] ?→6/9 · Polling job queue")
        keys(b"2")
        wait_for("Occurrence 1/2", "Cursor demo-09")
        keys(b"\x1b[C")
        wait_for("Occurrence 2/2", "Cursor demo-10")
        keys(b"3")
        wait_for("Occurrence 1/1", "Cursor demo-06")
        keys(b"1\x1b[C\r\r")
        wait_for("Paused", "outside filters", "Spike cursor demo-02", "Latest cursor demo-05")


def test_cross_service_context(binary, fixture):
    command = [binary, "--input", fixture, "--mute-unit", "indexer.service", "--no-color"]
    with terminal(command, columns=80, lines=24, env={"TZ": "UTC0"}) as (_, keys, wait_for):
        wait_for("11/min", "9/2/7")
        keys(b"\t\r1\x1b[Cc")
        screen = wait_for("Context ±2s", "11 retained", "2/11 · Cursor demo-02", "Backend latency above target")
        assert "›◆ 08:53:20.200 demo indexer INFO Polling job queue" in screen, screen
        keys(b"\x1b[B\x1b[B")
        wait_for("4/11 · Cursor demo-04", "›  08:53:20.600 demo proxy WARN")
        keys(b"\x1b")
        wait_for("Occurrence 2/6", "Cursor demo-02")
        keys(b"\rc")
        wait_for("Context ±2s", "2/11 · Cursor demo-02")
        keys(b"\x1b")
        wait_for("Paused", "outside filters", "Polling job queue")
        keys(b"g")
        wait_for("9/2/7", absent=("outside filters", "Polling job queue"))


def test_message_patterns(binary):
    command = [binary, "--input", "-", "--no-color"]
    with terminal(command, columns=80, lines=24, piped=True) as (scope, keys, wait_for):
        for req in ("183", "942"):
            scope.stdin.write(journal_record("api.service", 4, f"request {req} timed out", cursor=req).encode())
        scope.stdin.flush()
        wait_for("2/min", "request 942 timed out")
        keys(b"\t\r")
        wait_for("Top messages", "t patterns")
        keys(b"t")
        wait_for("Top patterns", "?→2/2 · request {id} timed out", "t exact")
        keys(b"1")
        wait_for("Occurrence 1/2", "request 183 timed out", "Cursor 183")
        keys(b"\x1b[C")
        wait_for("Occurrence 2/2", "request 942 timed out", "Cursor 942")
        keys(b"\x1b")
        wait_for("Top patterns")
        keys(b"t")
        wait_for("Top messages", "1/2 · request 183 timed out")


if __name__ == "__main__":
    test_default_live(sys.argv[1])
    test_replay(sys.argv[1])
    test_grouped_demo(sys.argv[1], sys.argv[2])
    test_jump_to_spike(sys.argv[1])
    test_compare_spike_drivers(sys.argv[1], sys.argv[2])
    test_tail_spike_hops(sys.argv[1])
    test_follow_ranked_message(sys.argv[1], sys.argv[2])
    test_cross_service_context(sys.argv[1], sys.argv[2])
    test_message_patterns(sys.argv[1])
