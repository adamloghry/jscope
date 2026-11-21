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

### Jump from a spike to its journal entry

Press **Tab** to focus services, **↑/↓** to choose a unit, then **Enter** to inspect its last minute. The graph holds twelve five-second intervals still while new records continue arriving. It starts on the newest red interval, then prefers the newest warning if there are no errors. For routine traffic it selects the busiest interval, favoring the more recent one on a tie.

Use **←/→** to select a bar. The preview shows the first error-or-higher entry in a red interval, the first warning in an amber interval, or the first arrival in a routine interval. Press **Enter** to pause the tail at that occurrence. If a display filter hid the entry, the tail temporarily reveals it and labels it **outside filters**. Press **Enter** again for its exact journal timestamp and cursor, including when the event is part of a repeat group. **g** returns to the filtered live tail.

**Esc** closes the inspector and returns to the service list. If a bar is empty or its original row has been evicted, you can pick another interval.

### Find what drove a spike

**Top messages** ranks the three most frequent messages in the selected service and interval. A row such as `1→6/9 · Polling job queue` means that exact text occurred once in the previous five seconds and six times among the selected interval's nine arrivals. The lookup includes every message in the preceding interval, even one that fell outside its top three. A `?` before the arrow means the earlier interval predates startup or overlaps missing intake history. An observed interval with no matches shows `0`. Repeats count individually across priorities and hosts. Search, solo and mute leave these counts intact. Ties sort by message text. The breakdown stays frozen as you scrub.

### Compare two services

In the service list, press **p** on a service to pin it, then **Enter** on a different service to see both graphs together. Pin the second service too if you want to keep the pair, including when either goes quiet. Open either pinned service to compare them again. Press **p** on a pin to release it.

Both graphs share the same capture time, five-second intervals and volume scale. Each shows the selected interval's count and the local journal time of its representative entry, so you can compare two warnings that landed in the same interval. Use **←/→** to scrub both and **↑/↓** to change which service supplies the message breakdown and jump target.

### Hop between warnings and errors in the tail

Press **[** from live to land on the newest five-second interval containing a warning or higher across all services. Press **[** again for the previous marked interval or **]** for the next. Each hop lands on the first incoming error or higher in that interval, or its first warning if it had no errors. The shortcuts also reveal hits hidden by a filter.

After an inspector jump, the brackets follow the service you jumped from. After manually selecting a log row, they search across services, starting on either side of the interval containing the row's first arrival. Navigation holds the captured minute still while intake continues. **g** returns to live, ready for a fresh capture.

## Try a synthetic replay

```sh
nix run . -- --input fixtures/demo.jsonl --mute-unit indexer.service
```

For the first 60 seconds, `indexer.service` leads with nine incoming entries, two visible, and seven suppressed. Its two warnings collapse into one row. The notice from `proxy.service` also stays visible. Press Enter to inspect the repeated warning. Press Tab to focus services and Shift+M to unmute the indexer.

To try **jump to spike** in this demo, press **Tab**, then **Enter** twice. The tail lands on the first “Job queue is backing up” warning. Enter opens details showing the matched cursor `demo-09` and the group's latest cursor `demo-10`.

To try the **two-service comparison** from startup, press **Tab**, **p** on the indexer, **↓** to the proxy, then **Enter**. Both services had warnings in the same interval. Switch to the indexer with **↓** to see that six of its nine arrivals say “Polling job queue”, even with the indexer muted.

To see the rising-rate indicator with synthetic input, send the fixture again after a quiet interval. Watch the indexer gain `↑` for five seconds when the second burst arrives. The warning bars from both bursts remain in the graph as they age.

```sh
nix build && { cat fixtures/demo.jsonl; sleep 18; cat fixtures/demo.jsonl; } | ./result/bin/jscope --input -
```

After the second burst arrives, press **[** twice from the tail. The first press lands in the newer warning interval, and the second takes you to the earlier burst. **]** takes you forward again.

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
| Enter | In the tail, expand or close details. In services, inspect the unit's graph. In the inspector, jump to the selected interval |
| p | Pin or unpin the selected service for comparison (up to two) |
| ← / → | Select an older or newer five-second interval in the inspector |
| ↑ / ↓ in a paired inspector | Switch the active service at the same interval |
| [ / ] in the tail | Jump to an older or newer warning/error interval in the captured minute |
| PgUp / PgDn | Scroll through expanded entry details |
| s | Solo the selected service or clear solo |
| m | Toggle muting below warning across all services |
| Shift+M | Toggle lower-severity muting for the selected sidebar service |
| / | Search unit, host, and message text without case sensitivity. Enter accepts, Esc restores the previous query, and Ctrl+U clears the query |
| Space | Pause or resume tail scrolling while intake continues |
| End or g | Jump to the live tail |
| Esc | Return from the inspector to services, cancel a search, or quit from the main view |
| q or Ctrl+C | Quit (while entering a search, use Ctrl+C) |

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
