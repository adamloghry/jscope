# JournalScope status

The integrator owns this status file and end-to-end verification. Ingestion, state/UI, and test packaging ran in parallel with disjoint file ownership. The executable is `jscope`.

| ID | Owner and files | State | Dependencies | Next action |
| --- | --- | --- | --- | --- |
| JS-01 | Integrator, STATUS.md and shared contract | verified | None | Use the contract for new fixtures and features |
| JS-02 | Ingestion, src/journal.rs and src/source.rs | verified | JS-01 | None |
| JS-03 | State/UI, src/state.rs and src/ui.rs | verified | JS-01 | None |
| JS-04 | Verification, tests/, fixtures/, nix/, and flake files | verified | JS-01, JS-02, JS-03 | None on x86_64-linux |
| JS-05 | Integrator, src/main.rs, src/lib.rs, Cargo files, and README.md | verified | JS-02, JS-03, JS-04 | None |
| JS-06 | Integrator, aarch64-linux execution | blocked | JS-04, aarch64 builder | Run the flake and VM checks on an aarch64-linux builder |
| JS-07 | Integrator, UI audit and regression | verified | JS-03 | None |
| JS-08 | Ingestion owner, journal metadata and fixtures | verified | JS-02 | None |
| JS-09 | State/UI owner, service controls and rendering | verified | JS-03 | None |
| JS-10 | Verification owner, terminal and VM tests | verified | JS-04 | None on x86_64-linux |
| JS-11 | Integrator, intake loop, CLI, docs and check-in | verified | JS-08–10 | None |
| JS-12 | State owner, startup visibility and model tests | verified | JS-03 | None |
| JS-13 | Time owner, local timestamp formatting | verified | JS-02 | None |
| JS-14 | Terminal-test owner, packaged interaction tests | verified | JS-12, JS-13 | None |
| JS-15 | Integrator, dashboard layout, docs and verification | verified | JS-12–14 | None on x86_64-linux |
| JS-16 | State owner, burst grouping | verified | JS-03 | None |
| JS-17 | UI owner, journal rows | verified | JS-16 | None |
| JS-18 | Test owner, core reference model and replay | verified | JS-16 | None |
| JS-19 | Integrator, CLI, terminal test, docs and check-in | verified | JS-16–18 | None on x86_64-linux |
| JS-20 | State owner, rate comparison and severity bins | verified | JS-03 | None |
| JS-21 | UI owner, activity strip and service panel | verified | JS-20 | None |
| JS-22 | Test owner, Hegel and replay | verified | JS-20 | None |
| JS-23 | Integrator, packaged terminal, docs and checks | verified | JS-20–22 | None on x86_64-linux |
| JS-24 | State owner, spike snapshots and tail anchors | verified | JS-16, JS-20 | None |
| JS-25 | UI owner, graph inspector and landing view | verified | JS-21, JS-24 | None |
| JS-26 | Test owner, model and replay tests | verified | JS-24 | None |
| JS-27 | Integrator, controls, PTY tests, docs and check-in | verified | JS-24–26 | None on x86_64-linux |

## Shared contract

Live input and replay fixtures are journalctl JSON, one object per line, with journal field names. Invalid lines can appear in regression fixtures. `journal::Event` stores optional journal realtime in microseconds, cursor, priority (0–7), and host, plus resolved unit, its journal source and other usable identity fields, and the sanitized message. The unit fallback starts with `_SYSTEMD_USER_UNIT`, followed by `_SYSTEMD_UNIT`, `SYSLOG_IDENTIFIER`, `_COMM`, and `(unknown)`. Duplicate fields use the first usable value. Journal byte arrays preserve valid UTF-8 and escape invalid bytes. Reader outcomes distinguish malformed and oversized lines, EOF, and source errors.

State receives monotonic intake times as milliseconds. The rolling window includes ages below 60 seconds. History retains journal timestamps. Adjacent valid events collapse by host, unit, priority, and message. The default display shows every priority. Global and per-service muting retain warnings, higher severities, and unknown priorities. Display controls act independently of intake. The UI shows incoming, visible, and suppressed occurrences, and reports any retention omissions.

The default burst grouping also folds exact routine repeats across other sources within five seconds and eight input positions of the first occurrence. Groups keep their first positions. Warnings and unknown priorities use adjacent-only folding. Pass `--adjacent-only` to use that rule for every group.

Graph bins keep the worst incoming severity. The rising arrow compares five seconds of intake to up to 60 preceding seconds within the existing activity memory budget. It becomes eligible after 15 seconds of observation, requires at least three arrivals and a twofold rate increase, and waits for complete comparison history after a capacity loss. The extra five seconds of retention are used only for that comparison.

Inspecting a service captures twelve five-second intake bins and at most one representative event per bin. A jump anchors the paused tail at the earliest occurrence of the displayed severity class. A filtered hit gets a temporary one-row reveal. A target evicted from tail history reports eviction, even if a later entry has identical text.

Replay has no control records or timing directives. Tests advance the state clock independently of journal timestamps. In the demo, `indexer.service` leads the first 60 seconds with 9 incoming, 2 visible, and 7 suppressed occurrences. Its paired warnings appear as one group.

## Recent decisions

- Use a 200-entry default live backlog and forward unit, boot, and priority selectors to journalctl. File and stdin replay use the same parser and state.
- Build Hegel's matching 0.51.6 native engine from locked crates inside the Nix sandbox. The pinned 0.52.7 frontend loads it via `HEGEL_C_LIB_DIR`.
- Use QEMU software acceleration for the VM check when KVM is unavailable. The guest exercises the packaged app under a regular account with journal access.

## Verification evidence

- `nix flake check -L` passed on x86_64-linux with sandboxing enabled. It ran rustfmt, strict Clippy across all targets, and 57 Rust tests including four Hegel properties, six replay tests, four retention regressions, and three subprocess tests. Cargo built offline. PTY checks ran the packaged app with no arguments against a synthetic follow process and with stdin connected to an open journal pipe. They verified the compact header, local time, filtering, service focus, pause, quit, and terminal restoration. No generated failure needed minimization. The ordinary parser regression fixture is in `fixtures/parser-regressions.jsonl`.
- `nix build .#vm-test --no-link -L` passed on x86_64-linux in a disposable NixOS VM. It verified real journalctl follow, matching unit and priority filters, exclusion of archived records from a prior guest boot, zero and nonzero backlogs, SIGTERM shutdown, child reaping, and restored terminal settings. All logs used for the test were synthetic guest records.
- The exact README replay command passed in an 80×24 pseudo-terminal. File and piped replay also passed direct PTY smoke tests, including details, unmute, clean quit, and terminal restoration with the pipe still open. `nix run . -- --help` verified the default app entry point.
- `nix flake check --all-systems --no-build` evaluated package, app, dev shell, standard checks, and VM check for x86_64-linux and aarch64-linux. The environment exposes x86_64 builders only, so aarch64-linux build and runtime tests remain open.

## Jump-to-spike verification

- The packaged app passed a piped 80×24 terminal test with a hidden error, a repeated copy and a later emergency in the same interval. New traffic changed the live rate while the inspectable bin stayed at four entries. The test covered cancel, an empty interval, jumping, exact and latest cursors, and return to the original filtered tail.
- The README demo keystrokes passed at 80×24 and 104×24. Both landed on the indexer's first warning, cursor `demo-09`, within the group ending at `demo-10`. Deterministic tests separately land on a repeat from an older group that spans multiple intake bins. A 150-case Hegel property compares every selectable bin against the individual original receipts, including evicted targets.
- The final sandboxed flake check and two-boot VM check passed on x86_64-linux. Both supported flake outputs evaluated. Native aarch64-linux execution remains tracked in JS-06.

## Activity indicator verification

- In the packaged 80×24 README demo, the first fixture produced 11/min and a steady indexer. After the 18-second gap, the second fixture produced 22/min, two distinct warning bars, and `↑ indexer.service`. The arrow cleared after five seconds while the bars and 18 indexer occurrences remained visible. Switching panes in a separate monochrome PTY run confirmed the proxy stayed unmarked.
- The reference model exercises severity and rate calculations on generated streams. An injected-clock replay verifies a rising worker below a steady higher-volume service, a fully filtered error, and the baseline sample aging out at exactly 65 seconds. Rendering tests verify actual foreground colors, monochrome symbols, and severity precedence when multiple bins share a terminal cell.

## Burst grouping verification

- The packaged 80×24 replay showed four interleaved indexer polls as `×4/800ms` in burst mode, with a separate proxy warning. In adjacent-only mode the first row showed `×3/400ms`. Both runs showed 11/min. The PTY tests opened each group, verified the full unit and latest cursor, and quit cleanly.
- The Hegel model compares both grouping modes against the same generated streams, including interleaved services, missing and reversed journal timestamps, filtering, pause and FIFO eviction. A regression verifies the history byte budget and occurrence totals when a 60 KiB record extends an earlier row.

## Release audit

- With a full synthetic backlog, the updated layout shows 20 log rows at 80×24 (previously 12) and 27 at 140×30 (previously 24). At 80 columns the busiest service occupies one line. Tab opens the full ranking. Wider terminals show the ranking next to the tail, using a shared vertical scale across service graphs.
- In the packaged live PTY regression, 31 info entries and one warning produced 32/min and 100% shown. Global mute preserved the warning and rate while the meter fell to 3%. The same test verified the event's local clock time under `TZ=JST-9`. Separate tests cover historical offsets on both sides of the spring and fall DST transitions, rolling graph expiry, and empty or paused views.

- The parser previously discarded the origin of the displayed service name and its remaining identity fields. These fields are now retained, with their allocations included in the history budget. A fixture also covers an invalid byte array that previously supplied a spurious priority.
- The baseline release needed 14.915 seconds to show the final warning of a 12,001-record synthetic replay through a pseudo-terminal. The main loop waited on keyboard input while replay records queued. The revised loop waits on the source and schedules terminal draws independently. The new Nix release binary displayed the final warning in 0.052 seconds using the same input and PTY on this machine. These are single-run measurements.
- `--mute-unit` and Shift+M now mute one service while retaining its warnings and intake rank. Native `--user-unit` selectors support user services. Expanded details can be scrolled through all retained fields, the latest cursor and the message. The sidebar keeps its activity counts visible for larger bursts.

## Earlier UI audit

- A long unit or hostname could consume the tail width and hide severity and message. The tail now budgets terminal cells for each field, shortening host and unit names while preserving severity and message space. A new 80-column regression covers two long hosts and a long unit.
