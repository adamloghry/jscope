"""Capture the README screenshots and the animated demo from a running jscope.

The real binary runs in XTerm on a synthetic journal stream while xdotool presses keys. Text is
rendered at 2x for sharpness, and every image gets the same frame as the terminals on
journalscope.akl.run (title bar, padding, border) so it reads on GitHub's light and dark themes.

Two sessions run, one after the other:
  stills  dashboard.png, investigation.png, context.png. Same keys and timing as the README
          describes, resizing the terminal per shot: 104 columns is the narrowest that shows the
          Services column (src/ui.rs); the inspector and context read best full-width at 90.
  gif     demo.gif. The same walk-through recorded at a fixed 104x24, with slower keys. It opens
          on the full dashboard, plays the keystrokes in real time, then the stream filling in
          at 4x before it loops.

From the repository root:
    nix run .#screenshots                 # both
    nix run .#screenshots -- --no-gif     # stills only (faster)

By hand: run under xvfb-run with XTerm, xdotool, ImageMagick 7, ffmpeg (with x11grab),
pngquant, oxipng and DejaVu Sans Mono.
"""

import argparse
from contextlib import contextmanager
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import tempfile
import time

OUT = Path(__file__).resolve().parent
BG = "#111923"  # terminal background, also the frame padding
BAR = "#1b2630"  # title bar, as on journalscope.akl.run
BORDER = "#3a4652"  # visible on GitHub's light and dark themes
TITLE = "#8b987b"
DOTS = ("#e5534b", "#d7b64b", "#3fa34d")
PALETTE = (
    "111923  f08088  7bc8a7  e5b567  82aee5  b39ad5  75c7ce  c5d0dd "
    "647988  ff999d  95dfbd  f1cb80  a0c7ff  d6b3f5  93dfe8  eff5fa"
).split()


def run(*args):
    return subprocess.check_output(args, text=True, timeout=120).strip()


def font_file():
    if os.environ.get("JSCOPE_CAPTURE_FONT"):
        return os.environ["JSCOPE_CAPTURE_FONT"]
    return run("fc-match", "-f", "%{file}", "DejaVu Sans Mono")


def frame(src, dest, title, font, scale=2):
    """Wrap a terminal image in the website's window chrome. `scale` is 2 for the 2x stills."""
    width = int(run("magick", "identify", "-format", "%w", str(src)))
    pad, bar, dot, gap, border = 14 * scale, 28 * scale, 9 * scale, 6 * scale, 1 * scale
    draw = []
    for i, colour in enumerate(DOTS):
        x = pad + i * (dot + gap)
        y = (bar - dot) // 2
        draw += [
            "-fill",
            colour,
            "-draw",
            f"rectangle {x},{y} {x + dot - 1},{y + dot - 1}",
        ]
    run(
        "magick",
        str(src),
        "-bordercolor",
        BG,
        "-border",
        str(pad),
        "(",
        "-size",
        f"{width + 2 * pad}x{bar}",
        f"xc:{BAR}",
        *draw,
        "-font",
        font,
        "-pointsize",
        str(12 * scale),
        "-fill",
        TITLE,
        "-gravity",
        "west",
        "-annotate",
        f"+{pad + 3 * (dot + gap) + 6 * scale}+0",
        title,
        ")",
        "+swap",
        "-append",
        "-bordercolor",
        BORDER,
        "-border",
        str(border),
        "-strip",
        str(dest),
    )


def optimise(png):
    """Terminal images are flat colours: a 256-colour palette is visually lossless and far smaller."""
    if shutil.which("pngquant"):
        subprocess.run(
            [
                "pngquant",
                "--force",
                "--skip-if-larger",
                "--quality",
                "85-100",
                "--output",
                str(png),
                str(png),
            ]
        )
    if shutil.which("oxipng"):
        run("oxipng", "-q", "-o", "4", "--strip", "safe", str(png))


class Session:
    def __init__(self, window, journal, tmp, font):
        self.window, self.journal, self.tmp, self.font = window, journal, tmp, font

    def keys(self, *names, gap=0.0):
        for name in names:
            run("xdotool", "key", "--clearmodifiers", name)
            time.sleep(gap)

    def resize(self, cols, rows):
        run(
            "xdotool",
            "windowsize",
            "--usehints",
            "--sync",
            self.window,
            str(cols),
            str(rows),
        )

    def snap(self, name, title):
        raw = self.tmp / f"raw-{name}"
        run("magick", "import", "-silent", "-window", self.window, "-strip", str(raw))
        frame(raw, OUT / name, title, self.font)
        optimise(OUT / name)

    def geometry(self):
        geo = dict(
            line.split("=")
            for line in run(
                "xdotool", "getwindowgeometry", "--shell", self.window
            ).splitlines()
        )
        return (
            int(geo["X"]),
            int(geo["Y"]),
            int(geo["WIDTH"]) // 2 * 2,
            int(geo["HEIGHT"]) // 2 * 2,
        )


@contextmanager
def session(binary, cols, rows, tmp, font):
    """jscope in XTerm, reading journal JSON from a FIFO that stays open until the session ends."""
    fifo = tmp / "journal"
    if fifo.exists():
        fifo.unlink()
    os.mkfifo(fifo)
    terminal = [
        "xterm",
        "-T",
        "JournalScope capture",
        "-geometry",
        f"{cols}x{rows}",
        "-fa",
        "DejaVu Sans Mono",
        "-fs",
        "28",  # 2x the 14 pt the README was first sized for
        "-b",
        "0",
        "-u8",
        "-bg",
        BG,
        "-fg",
        "#cfdae4",
        "+sb",
    ]
    for i, colour in enumerate(PALETTE):
        terminal += ["-xrm", f"XTerm*color{i}: #{colour}"]
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
            "XDG_CONFIG_HOME": str(tmp),
        },
    )
    journal = None
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
        journal = fifo.open("w")  # kept open so jscope stays in "Live replay"
        s = Session(window, journal, tmp, font)
        yield s
        s.keys("q")
        proc.wait(timeout=10)
    finally:
        if journal:
            journal.close()
        if proc.poll() is None:
            proc.terminate()
            proc.wait(timeout=5)


def stills(binary, tmp, font):
    with session(binary, 104, 24, tmp, font) as s:
        stream_synthetic_journal(s.journal)
        s.snap("dashboard.png", "jscope · dashboard")
        # Metrics ranks first. Pin the next two services and inspect checkout-api.
        s.keys("Tab", "Down", "p", "Down", "p", "Up", "Return", "t")
        s.resize(90, 16)
        time.sleep(0.5)
        s.snap("investigation.png", "jscope · spike inspector")
        # Follow the first request in the second-ranked pattern, then its journal context.
        s.keys("2", gap=0.25)
        s.keys("c", gap=0.25)
        s.resize(90, 21)
        time.sleep(0.5)
        s.snap("context.png", "jscope · context")


def demo_gif(binary, tmp, font):
    with session(binary, 104, 24, tmp, font) as s:
        x, y, w, h = s.geometry()
        video = tmp / "session.mkv"
        recorder = subprocess.Popen(
            [
                "ffmpeg",
                "-loglevel",
                "error",
                "-y",
                "-f",
                "x11grab",
                "-draw_mouse",
                "0",
                "-framerate",
                "15",
                "-video_size",
                f"{w}x{h}",
                "-i",
                f"{os.environ['DISPLAY']}+{x},{y}",
                "-c:v",
                "libx264rgb",
                "-crf",
                "0",
                "-preset",
                "ultrafast",
                str(video),
            ],
            stdin=subprocess.PIPE,
        )
        try:
            time.sleep(1)
            if recorder.poll() is not None:
                raise SystemExit(
                    "ffmpeg could not record the X display (does it have x11grab?)"
                )
            started = time.monotonic()
            stream_synthetic_journal(s.journal)
            stream_done = (
                time.monotonic() - started + 1
            )  # +1: the recorder started a second earlier
            time.sleep(2.5)
            s.keys("Tab", "Down", "p", "Down", "p", "Up", "Return", gap=0.45)
            time.sleep(1.5)
            s.keys("t")
            time.sleep(3.0)
            s.keys("2", gap=1.5)
            s.keys("c")
            time.sleep(3.5)
            recorder.communicate(b"q", timeout=60)
        finally:
            if recorder.poll() is None:
                recorder.send_signal(signal.SIGINT)
                recorder.wait(timeout=10)
    make_gif(video, stream_done, w // 2, h // 2, font, tmp)


def make_gif(video, stream_done, w, h, font, tmp):
    """The keystrokes in real time, then the stream filling in at 4x, framed at 1x and palettised.

    The loop starts on the full dashboard rather than an empty "Waiting for journal entries" screen:
    GitHub shows only the first frame, paused, to visitors who prefer reduced motion.
    """
    blank = tmp / "blank.png"
    run("magick", "-size", f"{w}x{h}", f"xc:{BG}", str(blank))
    chrome = tmp / "chrome.png"
    frame(blank, chrome, "jscope · demo", font, scale=1)
    cut = f"{stream_done:.2f}"
    graph = (
        f"[0:v]split[a][b];"
        f"[a]trim=0:{cut},setpts=(PTS-STARTPTS)/4[fast];"
        f"[b]trim=start={cut},setpts=PTS-STARTPTS[real];"
        f"[real][fast]concat=n=2:v=1[v];"
        f"[v]scale={w}:{h}:flags=lanczos[s];"
        # 1 border + 14 padding (+ 28 title bar). fps after the overlay: the looped frame image would set 25.
        f"[1:v][s]overlay=x=15:y=43:shortest=1,fps=12,split[o1][o2];"
        f"[o1]palettegen=max_colors=64:stats_mode=diff[p];"
        f"[o2][p]paletteuse=dither=none:diff_mode=rectangle"
    )
    run(
        "ffmpeg",
        "-loglevel",
        "error",
        "-y",
        "-i",
        str(video),
        "-loop",
        "1",
        "-i",
        str(chrome),
        "-filter_complex",
        graph,
        "-loop",
        "0",
        str(OUT / "demo.gif"),
    )


def stream_synthetic_journal(stream):
    """Five batches 5.2 s apart, ending in a timeout burst (the stream the README describes)."""
    number = 0

    def emit(unit, level, message):
        nonlocal number
        number += 1
        record = {
            "__REALTIME_TIMESTAMP": str(1_773_324_900_000_000 + number * 73_000),
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
        emit("inventory", 6, f"Stock reservation committed for order {8230 + batch}")
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
                    emit("worker", 6, "Lease renewed for order reconciliation")
                if n == 3:
                    emit("metrics", 4, "Export queue delayed for 14 seconds")
            emit("postgresql", 3, "Transaction canceled after lock timeout")
            emit("nginx", 4, "Upstream response exceeded 2.0 seconds")
            emit("queue-worker", 6, "Deferred job scheduled for retry")
        stream.flush()
        time.sleep(5.2 if batch < 4 else 0.5)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    parser.add_argument("binary", help="path to the jscope executable")
    parser.add_argument(
        "--no-gif", dest="gif", action="store_false", help="skip recording demo.gif"
    )
    parser.add_argument(
        "--keep",
        action="store_true",
        help="keep raw shots and the recording for debugging",
    )
    args = parser.parse_args()
    binary = shutil.which(args.binary)
    if not binary:
        raise SystemExit("Pass the path to the jscope executable")
    font = font_file()
    with tempfile.TemporaryDirectory(delete=not args.keep) as tmp:
        if args.keep:
            print(f"keeping intermediate files in {tmp}", file=sys.stderr)
        stills(os.path.realpath(binary), Path(tmp), font)
        if args.gif:
            demo_gif(os.path.realpath(binary), Path(tmp), font)
