"""Drive the packaged JournalScope binary through a PTY inside the guest."""

import errno
import os
from pathlib import Path
import pty
import select
import signal
import socket
import subprocess
import sys
import termios
import time


def wait_for(predicate, drain, timeout=20):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        result = predicate()
        if result:
            return result
        drain(0.1)
    raise AssertionError(f"timed out waiting for {predicate}")


def main():
    binary, phase, backlog, *options = sys.argv[1:]
    master, slave = pty.openpty()
    termios.tcsetwinsize(slave, (30, 150))
    initial_terminal = termios.tcgetattr(slave)
    output = bytearray()
    notify_path = Path.cwd() / "notify.sock"
    notifier = socket.socket(socket.AF_UNIX, socket.SOCK_DGRAM)
    notifier.bind(os.fspath(notify_path))
    notifier.setblocking(False)

    def drain(timeout):
        readable, _, _ = select.select([master], [], [], timeout)
        if readable:
            try:
                output.extend(os.read(master, 65536))
            except OSError as error:
                if error.errno != errno.EIO:
                    raise

    with subprocess.Popen(
        [
            binary,
            "--unit=jscope-target.service",
            "--boot=0",
            "--priority=err",
            *([] if options == ["default"] else [f"--backlog={backlog}"]),
            "--no-color",
        ],
        stdin=slave,
        stdout=slave,
        stderr=slave,
        env={
            **os.environ,
            "TERM": "xterm",
            "PATH": "/run/current-system/sw/bin",
            "NOTIFY_SOCKET": os.fspath(notify_path),
        },
    ) as scope:

        def journal_child():
            children = Path(f"/proc/{scope.pid}/task/{scope.pid}/children").read_text()
            for pid in children.split():
                try:
                    args = Path(f"/proc/{pid}/cmdline").read_bytes().split(b"\0")
                except FileNotFoundError:
                    continue
                if Path(os.fsdecode(args[0])).name == "journalctl":
                    return int(pid), args
            return None

        def follow_ready():
            try:
                return b"READY=1" in notifier.recv(4096).splitlines()
            except BlockingIOError:
                return False

        try:
            journal_pid, args = wait_for(journal_child, drain)
            for expected in (b"--follow", b"--output=json", b"--no-pager"):
                assert expected in args, args
            for option, expected in (
                (b"--unit", b"jscope-target.service"),
                (b"--boot", b"0"),
                (b"--priority", b"err"),
                (b"--lines", backlog.encode()),
            ):
                assert option + b"=" + expected in args or (
                    option in args and args[args.index(option) + 1] == expected
                ), args
            wait_for(lambda: b"\x1b[?1049h" in output, drain)
            wait_for(follow_ready, drain)
            if backlog != "0":
                wait_for(lambda: b"JSCOPE_PREEXISTING_KEEP" in output, drain)
            Path("ready").touch()
            expected_follow = f"JSCOPE_{phase}_KEEP".encode()
            wait_for(lambda: expected_follow in output, drain)
            scope.send_signal(signal.SIGTERM)
            wait_for(lambda: scope.poll() is not None, drain)
            assert scope.returncode == 0, scope.returncode
            drain(0)
            for rejected in (
                b"JSCOPE_PRIOR_BOOT_KEEP",
                b"JSCOPE_PREEXISTING_PRIORITY_EXCLUDED",
                f"JSCOPE_{phase}_PRIORITY_EXCLUDED".encode(),
                b"JSCOPE_UNIT_EXCLUDED",
            ):
                assert rejected not in output, rejected
            if backlog == "0":
                assert b"JSCOPE_PREEXISTING_KEEP" not in output
                assert b"JSCOPE_BACKLOG_FOLLOW_KEEP" not in output
            assert not Path(f"/proc/{journal_pid}").exists(), "journalctl was not reaped"
            assert termios.tcgetattr(slave) == initial_terminal, "PTY settings changed"
            Path("passed").touch()
        except BaseException:
            print(output[-8000:].decode(errors="backslashreplace").encode("unicode_escape"))
            raise
        finally:
            if scope.poll() is None:
                scope.kill()
    notifier.close()
    os.close(master)
    os.close(slave)


if __name__ == "__main__":
    main()
