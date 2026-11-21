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


def journal_record(unit, level, message, realtime_us=0):
    record = {
        "_SYSTEMD_UNIT": unit,
        "PRIORITY": str(level),
        "_HOSTNAME": "fixture",
        "MESSAGE": message,
        "__REALTIME_TIMESTAMP": str(realtime_us),
    }
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
            keys(b"\x1b[A" * 10 + b"\r")
            wait_for("_SYSTEMD_UNIT=indexer.service", f"Latest cursor {latest_cursor}")


if __name__ == "__main__":
    test_default_live(sys.argv[1])
    test_replay(sys.argv[1])
    test_grouped_demo(sys.argv[1], sys.argv[2])
