# JournalScope

`jscope` is a Linux terminal dashboard for finding which systemd service just got noisy. It follows structured journal output and ranks services by how many entries they produced in the last 60 seconds.

## Run live

```sh
nix run .
```

Run from the repository root in an interactive Linux terminal with Nix flakes enabled. Live mode needs `journalctl` on your path and uses your existing journal access. To build the executable, run `nix build` followed by `./result/bin/jscope`. Use `nix run . -- --help` to list the options. Live mode starts with the most recent 200 matching entries, then follows new entries. Pass `--backlog 0` to start with new entries only.

```sh
nix run . -- --backlog 50 --boot --unit sshd.service --priority debug
```

Use `--unit` for system services or `--user-unit` for user services. Both options may be repeated. `--boot` selects the current boot, and accepts a boot ID or offset such as `--boot=-1`. `--priority` accepts journalctl names, numbers, or inclusive ranges. A single level includes all higher severities. Live mode passes these options directly to journalctl. With file or stdin replay, apply native selectors to the journalctl command that produces the JSON. [journalctl manual](https://www.freedesktop.org/software/systemd/man/latest/journalctl.html)

**The tail starts with all priorities visible.** Press `m` to mute entries below warning across all services, or select a service and press `Shift+M` to mute just that service. Warnings, higher severities, and entries with unknown priority pass either mute.

**The dashboard counts only events journalctl sends, before JournalScope's display filters.** The header graph shows the last minute of intake, oldest on the left. At full width each column covers five seconds. The number beside it counts arrivals over that minute, including the startup backlog as it arrives. The segmented meter shows the percentage of those occurrences that pass the current display filters. Repeats count individually, even while the tail is paused.

The service list ranks units by incoming activity. The service graphs share a scale so you can compare bursts across rows. Each row shows its trend and three counts in the order incoming / visible / suppressed. On a narrow terminal, the busiest service stays above the full-width tail. Press Tab for the complete ranking or to return to the log. At 104 columns and wider, the ranking stays beside the log.

- **↑ beside a unit** means it received at least three entries in the last five seconds, at a rate at least twice its average over the preceding minute. After 15 seconds of observation, the arrow can use the portion of that minute already observed. A previously quiet unit can qualify. The comparison pauses if its history is incomplete after a buffer eviction.
- **Amber and red bars** mark time intervals containing a warning or an error, respectively. Critical, alert and emergency entries also turn the bar red. Height still shows volume, and the most severe entry sets the color. With `--no-color`, `!` marks a warning and `×` marks an error or higher. These markers follow incoming events even while a search, mute or solo hides the corresponding log rows.

## Try a synthetic replay

```sh
nix run . -- --input fixtures/demo.jsonl --mute-unit indexer.service
```

For the first 60 seconds, `indexer.service` leads with nine incoming entries, two visible, and seven suppressed. Its two warnings collapse into one row. The notice from `proxy.service` also stays visible. Press Enter to inspect the repeated warning. Press Tab to focus services and Shift+M to unmute the indexer.

To see the rising-rate indicator with synthetic input, send the fixture again after a quiet interval. Watch the indexer gain `↑` for five seconds when the second burst arrives. The warning bars from both bursts remain in the graph as they age.

```sh
nix build && { cat fixtures/demo.jsonl; sleep 18; cat fixtures/demo.jsonl; } | ./result/bin/jscope --input -
```

To see burst grouping, run `nix run . -- --input fixtures/demo.jsonl`. Four “Polling job queue” entries share one `indexer` row marked `×4/800ms`, and the interleaved proxy warning has its own row. Add `--adjacent-only` to see where the warning interrupted the polling stream.

Each `--mute-unit NAME` selects an exact unit name for lower-severity muting at startup. A service mute stays in effect when you toggle global mute with `m`. The same controls work with live, file, and piped input.

Replay reads the same one-object-per-line JSON as live mode, as quickly as input arrives. The activity window follows receipt time. The tail and repeat details show journal timestamps in the viewer's local time zone, using the offset in effect at the time of each event. The tail stays available after the replay finishes. For an SSH stream, run this from a local terminal.

```sh
ssh HOST journalctl --follow --output=json --no-pager --lines=100 | nix run . -- --input -
```

## Controls

| Key | Action |
| --- | --- |
| Tab | Switch focus between tail and services |
| ↑ / ↓ | Select a group or a service |
| Enter | Expand or close the selected tail group |
| PgUp / PgDn | Scroll through expanded entry details |
| s | Solo the selected service or clear solo |
| m | Toggle muting below warning across all services |
| Shift+M | Toggle lower-severity muting for the selected sidebar service |
| / | Search unit, host, and message text without case sensitivity. Enter accepts, Esc restores the previous query, and Ctrl+U clears the query |
| Space | Pause or resume tail scrolling while intake continues |
| End or g | Jump to the live tail |
| q, Esc, or Ctrl+C | Quit |

## Reading the journal tail

The tail shows `sshd.service` as `sshd` and keeps other suffixes such as `.socket` and `.timer`. The activity panel, details and unit filters use the full name. JournalScope prefers `_SYSTEMD_USER_UNIT`, followed by `_SYSTEMD_UNIT`, `SYSLOG_IDENTIFIER`, and `_COMM`. Entries without any of these share the `(unknown)` bucket. The reader uses the first usable value for each field and escapes control characters and invalid bytes for display.

Burst grouping folds exact repeats from a named unit at notice, info or debug priority even when other services write in between. A different message or priority from the same host and unit starts a new group. A repeat can bridge an interruption within five seconds and eight input positions of the first occurrence. Journal timestamps, when available, must also fall in that interval and progress forward or stay equal. The row stays at its first occurrence and shows the count and elapsed span. Press Enter to inspect the timestamps and latest cursor.

Consecutive exact repeats at any priority always fold. Warnings, higher severities and unknown priorities use this consecutive-only rule. The gutter connects neighboring routine rows from the same host and unit. Pass `--adjacent-only` with either live or replay input for a timeline that groups only consecutive exact repeats.

Use `--no-color` for an uncolored display. Expanded details identify the journal fields that supplied the unit names. All dashboard history and mute settings live in memory for the current session. A diagnostic line appears when input is malformed or oversized, history is evicted, activity is dropped, or the source reports a problem. The tail holds up to 2,000 groups, and activity holds up to 16,384 intake entries, with a separate 12 MiB text budget for each. If the activity buffer reaches a limit before entries expire, its counts reflect the retained window. Journal lines over 64 KiB are discarded before their complete contents can be buffered. Journalctl can emit a null value for large fields, which the viewer treats as unavailable.

## Develop and verify

```sh
nix develop
nix flake check
nix build .#vm-test -L
```

The standard checks run rustfmt, Clippy, Hegel properties, deterministic replay and subprocess tests, and the packaged app in a pseudo-terminal with a separate journal pipe. The pinned Hegel frontend loads the matching native engine built from locked dependencies via `HEGEL_C_LIB_DIR`. [Hegel Rust documentation](https://docs.rs/hegeltest/0.52.7/hegel/)

The VM check writes synthetic service logs inside a disposable NixOS guest, reboots it, and verifies boot selection, unit and priority filtering, backlog, follow, and shutdown. See [STATUS.md](STATUS.md) for the results and architectures exercised.
