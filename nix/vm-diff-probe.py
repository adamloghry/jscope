"""Compare real local closures as the journal-reading observer in the guest."""

import os
import re
import runpy
import subprocess
import sys


def main():
    terminal_helper, binary, base, special, version = sys.argv[1:]
    assert os.getuid() != 0
    terminal = runpy.run_path(terminal_helper)["terminal"]
    # Wait for this record before jumping. A partial initial backlog could
    # otherwise contain the boot marker without the later switch and rollback.
    def announce(message):
        subprocess.run(["logger", "-p", "user.notice", "-t", "jscope-vm", message], check=True)

    announce("JSCOPE_AFTER_ROLLBACK")
    command = [binary, "--boot=0", "--priority=notice", "--backlog=1000", "--no-color"]
    with terminal(command, columns=160, lines=50) as (_, keys, wait):
        wait("JSCOPE_AFTER_ROLLBACK")
        keys(b"Gd")
        for number, (old, new, before, after) in enumerate(
            [(special, base, "∅", version), (base, special, version, "∅")]
        ):
            screen = wait("NixOS · closure changes", "hello:", version)
            assert f"From {old}" in screen and f"To   {new}" in screen, screen
            row = next(line for line in screen.splitlines() if "hello:" in line)
            assert re.search(rf"hello:.*{re.escape(before)}.*→.*{re.escape(after)}", row), row
            if number == 0:
                keys(b"d")
                wait("◆ NixOS", absent=("closure changes",))
                keys(b"Gd")

        # Live intake continues while the diff is open. Return directly to live.
        announce("JSCOPE_DURING_DIFF")
        keys(b"g")
        wait("Live", "JSCOPE_DURING_DIFF", absent=("closure changes",))


if __name__ == "__main__":
    main()
