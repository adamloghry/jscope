"""Photograph a running jscope in XTerm using synthetic journal input.

Requires Python 3, XTerm, xdotool, ImageMagick and DejaVu Sans Mono.
With these dependencies on PATH, run from the repository root:
    xvfb-run -a -s "-screen 0 2000x1000x24 -nolisten tcp" \
        python3 docs/screenshots/capture.py /path/to/jscope
"""

import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import time


def run(*args):
    return subprocess.check_output(args, text=True, timeout=12).strip()


def capture(binary):
    out = Path(__file__).resolve().parent
    palette = (
        "111923  f08088  7bc8a7  e5b567  82aee5  b39ad5  75c7ce  c5d0dd "
        "647988  ff999d  95dfbd  f1cb80  a0c7ff  d6b3f5  93dfe8  eff5fa"
    ).split()
    with tempfile.TemporaryDirectory() as tmp:
        fifo = Path(tmp) / "journal"
        os.mkfifo(fifo)
        terminal = [
            "xterm",
            "-T",
            "JournalScope capture",
            "-geometry",
            "126x24",
            "-fa",
            "DejaVu Sans Mono",
            "-fs",
            "14",
            "-b",
            "16",
            "-u8",
            "-bg",
            "#111923",
            "-fg",
            "#cfdae4",
            "+sb",
        ]
        for i, color in enumerate(palette):
            terminal.extend(["-xrm", f"XTerm*color{i}: #{color}"])
        # The terminal remains the app's controlling TTY while the FIFO supplies journal JSON.
        reader = "import os,sys; os.dup2(os.open(sys.argv[1],os.O_RDONLY),0); os.execv(sys.argv[2],sys.argv[2:])"
        proc = subprocess.Popen(
            [
                *terminal,
                "-e",
                sys.executable,
                "-c",
                reader,
                str(fifo),
                binary,
                "--no-defaults",
                "--input",
                "-",
                "--mute-unit",
                "metrics.service",
            ],
            env={
                **os.environ,
                "TZ": "UTC0",
                "LC_ALL": "C.UTF-8",
                "NO_COLOR": "",
                "XDG_CONFIG_HOME": tmp,
            },
        )
        try:
            window = run(
                "xdotool",
                "search",
                "--onlyvisible",
                "--sync",
                "--name",
                "^JournalScope capture$",
            ).splitlines()[0]
            run("xdotool", "windowfocus", "--sync", window)
            with fifo.open("w") as stream:
                number = 0

                def emit(unit, level, message):
                    nonlocal number
                    number += 1
                    record = {
                        "__REALTIME_TIMESTAMP": str(
                            1_773_324_900_000_000 + number * 73_000
                        ),
                        "__CURSOR": f"demo-{number:04}",
                        "_HOSTNAME": "edge-01",
                        "_SYSTEMD_UNIT": f"{unit}.service",
                        "PRIORITY": str(level),
                        "MESSAGE": message,
                    }
                    print(json.dumps(record), file=stream)

                for batch, (queries, retries) in enumerate(
                    [(2, 1), (5, 2), (4, 4), (9, 7), (6, 18)]
                ):
                    emit("nginx", 6, "GET /healthz 200")
                    emit("redis", 6, "Snapshot completed in 18 ms")
                    for _ in range(6 + 2 * batch):
                        emit("metrics", 7, "Collecting node metrics")
                    for _ in range(queries):
                        emit("postgresql", 6, "Waiting for checkout transaction")
                    emit(
                        "inventory",
                        6,
                        f"Stock reservation committed for order {8230 + batch}",
                    )
                    if batch >= 2:
                        emit("postgresql", 4, "Lock wait exceeded 500 ms on orders")
                    for _ in range(retries):
                        emit("checkout-api", 6, "Retrying checkout transaction")
                    emit("shipping", 6, f"Published dispatch batch {412 + batch}")
                    if batch >= 1:
                        emit("checkout-api", 4, "Connection pool nearly exhausted")
                    if batch == 4:
                        for n in range(6):
                            emit("checkout-api", 3, f"Request {7320 + n} timed out")
                            if n == 1:
                                emit(
                                    "worker",
                                    6,
                                    "Lease renewed for order reconciliation",
                                )
                            if n == 3:
                                emit(
                                    "metrics", 4, "Export queue delayed for 14 seconds"
                                )
                        emit("postgresql", 3, "Transaction canceled after lock timeout")
                        emit("nginx", 4, "Upstream response exceeded 2.0 seconds")
                        emit("queue-worker", 6, "Deferred job scheduled for retry")
                    stream.flush()
                    time.sleep(5.2 if batch < 4 else 0.5)

                def snap(name):
                    run(
                        "magick",
                        "import",
                        "-silent",
                        "-window",
                        window,
                        "-strip",
                        str(out / name),
                    )

                snap("dashboard.png")
                # Metrics ranks first. Pin the next two services and inspect checkout-api.
                run(
                    "xdotool",
                    "key",
                    "--clearmodifiers",
                    "Tab",
                    "Down",
                    "p",
                    "Down",
                    "p",
                    "Up",
                    "Return",
                    "t",
                )
                run("xdotool", "windowsize", "--usehints", "--sync", window, "90", "16")
                time.sleep(0.5)
                snap("investigation.png")
                # Follow the first request in the second-ranked pattern, then its journal context.
                run("xdotool", "key", "--clearmodifiers", "2")
                time.sleep(0.25)
                run("xdotool", "key", "--clearmodifiers", "c")
                time.sleep(0.25)
                run(
                    "xdotool", "windowsize", "--usehints", "--sync", window, "102", "20"
                )
                time.sleep(0.5)
                snap("context.png")
                run("xdotool", "key", "q")
            proc.wait(timeout=5)
        finally:
            if proc.poll() is None:
                proc.terminate()
                proc.wait(timeout=5)


if __name__ == "__main__":
    binary = shutil.which(sys.argv[1])
    if not binary:
        raise SystemExit("Pass the path to the jscope executable")
    capture(os.path.realpath(binary))
