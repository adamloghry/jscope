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

**The activity panel counts only events journalctl sends, before JournalScope's display filters.** The incoming count and sparkline use the last 60 seconds of intake, including the startup backlog as it arrives. Visible and suppressed split those occurrences using the current display filters, independently of scrolling, repeat collapse, and tail eviction. Lower-severity muting starts enabled. Warning and higher levels, and entries with unknown priority, pass that mute.

## Try a synthetic replay

```sh
nix run . -- --input fixtures/demo.jsonl --mute-unit indexer.service
```

For the first 60 seconds, `indexer.service` leads with nine incoming entries, two visible, and seven suppressed. Its two warnings collapse into one row. The notice from `proxy.service` also stays visible. Press Enter to inspect the repeated warning. Press Tab to focus services and Shift+M to unmute the indexer.

Each `--mute-unit NAME` selects an exact unit name for lower-severity muting at startup. Supplying any of these options starts with other units visible at all priorities. A service mute stays in effect when you toggle global mute with `m`. The same controls work with live, file, and piped input.

Replay reads the same one-object-per-line JSON as live mode, as quickly as input arrives. The activity window follows receipt time and the tail shows UTC journal timestamps. The tail stays available after the replay finishes. For an SSH stream, run this from a local terminal.

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

For the name in the tail and activity panel, JournalScope prefers `_SYSTEMD_USER_UNIT`, followed by `_SYSTEMD_UNIT`, `SYSLOG_IDENTIFIER`, and `_COMM`. Entries without any of these share the `(unknown)` bucket. The reader uses the first usable value for each field and escapes control characters and invalid bytes for display. Expanded details identify the journal fields that supplied the unit names. All dashboard history and mute settings live in memory for the current session.

Use `--no-color` for an uncolored display. Adjacent valid entries with the same host, resolved unit, priority, and message collapse into a group with an occurrence count and journal time span. The status line reports malformed and oversized input, tail eviction, early activity omissions, and source diagnostics. The tail holds up to 2,000 groups, and activity holds up to 16,384 intake entries, with a separate 12 MiB text budget for each. If the activity buffer reaches a limit before entries expire, its counts reflect the retained window. Journal lines over 64 KiB are discarded before their complete contents can be buffered. Journalctl can emit a null value for large fields, which the viewer treats as unavailable.

## Develop and verify

```sh
nix develop
nix flake check
nix build .#vm-test -L
```

The standard checks run rustfmt, Clippy, Hegel properties, deterministic replay and subprocess tests, and the packaged app in a pseudo-terminal with a separate journal pipe. The pinned Hegel frontend loads the matching native engine built from locked dependencies via `HEGEL_C_LIB_DIR`. [Hegel Rust documentation](https://docs.rs/hegeltest/0.52.7/hegel/)

The VM check writes synthetic service logs inside a disposable NixOS guest, reboots it, and verifies boot selection, unit and priority filtering, backlog, follow, and shutdown. See [STATUS.md](STATUS.md) for the results and architectures exercised.
