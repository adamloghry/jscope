"""Exercise piped journal replay and a separate controlling terminal."""

import fcntl
import json
import os
import pty
import select
import subprocess
import sys
import termios
import time

import pyte


def test_replay(binary, master, slave):
    screen = pyte.Screen(columns=140, lines=30)
    stream = pyte.ByteStream(screen)
    termios.tcsetwinsize(slave, (screen.lines, screen.columns))
    initial_settings = termios.tcgetattr(slave)

    # PTY input and the journal pipe are independent even when stdin is redirected.
    def claim_terminal():
        os.setsid()
        fcntl.ioctl(slave, termios.TIOCSCTTY, 0)

    with subprocess.Popen(
        [binary, "--input", "-", "--no-color", "--mute-unit", "loud.service"],
        stdin=subprocess.PIPE,
        stdout=slave,
        stderr=slave,
        pass_fds=(slave,),
        preexec_fn=claim_terminal,
        env={**os.environ, "TERM": "xterm"},
    ) as scope:
        terminal_output = bytearray()

        def drain(timeout):
            if select.select([master], [], [], timeout)[0]:
                data = os.read(master, 65536)
                stream.feed(data)
                terminal_output.extend(data)

        def wait_for(*expected):
            deadline = time.monotonic() + 10
            while time.monotonic() < deadline and scope.poll() is None:
                contents = "\n".join(screen.display)
                if all(text in contents for text in expected):
                    return contents
                drain(0.1)
            raise AssertionError(
                f"Waiting for {expected}; exit status {scope.poll()}\n"
                + "\n".join(screen.display)
            )

        def journal_record(unit, level, message):
            record = {
                "_SYSTEMD_UNIT": unit,
                "PRIORITY": str(level),
                "_HOSTNAME": "fixture",
                "MESSAGE": message,
            }
            scope.stdin.write((json.dumps(record) + "\n").encode())
            scope.stdin.flush()

        try:
            wait_for("JournalScope", "replay")
            journal_record("loud.service", 4, "ANCHOR_WATCH")
            journal_record("quiet.service", 6, "QUIET_INFO_WATCH")
            journal_record("loud.service", 6, "LOUD_HIDDEN")
            contents = wait_for("ANCHOR_WATCH", "QUIET_INFO_WATCH", "2/1/1", "In 3")
            assert "LOUD_HIDDEN" not in contents

            os.write(master, b" ")
            wait_for("Paused")
            journal_record("loud.service", 4, "LATER_WATCH")
            journal_record("loud.service", 6, "LOUD_MORE_HIDDEN")
            contents = wait_for("Paused", "In 5", "4/2/2", "ANCHOR_WATCH", "QUIET_INFO_WATCH")
            assert "LATER_WATCH" not in contents

            os.write(master, b"/watch\r")
            wait_for(" / watch")
            journal_record("loud.service", 4, "WATCH_SEARCH_NEW")
            journal_record("quiet.service", 6, "QUIET_FILTERED")
            contents = wait_for("Paused", "In 7", "5/3/2", "2/1/1", "ANCHOR_WATCH")
            assert "WATCH_SEARCH_NEW" not in contents
            assert "QUIET_FILTERED" not in contents

            os.write(master, b"g")
            contents = wait_for("Live", "LATER_WATCH", "WATCH_SEARCH_NEW", "QUIET_INFO_WATCH")
            assert "QUIET_FILTERED" not in contents
            os.write(master, b"/\x15\r")
            wait_for("QUIET_FILTERED")

            os.write(master, b"\tM")
            wait_for("LOUD_HIDDEN", "LOUD_MORE_HIDDEN", "5/5/0", "QUIET_FILTERED")
            os.write(master, b"M")
            contents = wait_for("5/3/2", "QUIET_FILTERED")
            assert "LOUD_HIDDEN" not in contents
            assert "LOUD_MORE_HIDDEN" not in contents

            # Quit without closing stdin so shutdown must interrupt the waiting reader.
            os.write(master, b"q")
            assert scope.wait(timeout=10) == 0
            while select.select([master], [], [], 0)[0]:
                drain(0)
            assert b"\x1b[?1049l" in terminal_output
            assert termios.tcgetattr(slave) == initial_settings
        finally:
            if scope.poll() is None:
                scope.kill()


if __name__ == "__main__":
    master_fd, slave_fd = pty.openpty()
    try:
        test_replay(sys.argv[1], master_fd, slave_fd)
    finally:
        os.close(master_fd)
        os.close(slave_fd)
