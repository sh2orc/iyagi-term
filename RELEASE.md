# Release notes

Installers live in [releases/](releases/) and are linked from the [README](README.md#download)
with their SHA-256. How releases are built, signed and notarized:
[releases/README.md](releases/README.md).

## 0.1.1 — 2026-10-10

A resource-governance release: CPU contention is now shared rather than frozen, memory
crises are judged by the kernel instead of a fixed ratio, and everything the guard freezes
for memory comes back on its own once the host recovers.

### Resource guard & pressure

- **CPU hogs yield instead of freezing.** Going over the per-terminal CPU limit is no longer
  a reason to suspend: builds and tests are supposed to use CPU, and freezing also cut the
  agent's API calls. A terminal that stays over the limit (6 cores for 20 s by default) drops
  to a lower scheduling tier, regardless of host pressure, and is restored once the overage
  ends. Focused, protected and manually handled terminals are never yielded.
- **Memory is the only freeze reason, and only under pressure.** The per-terminal memory limit
  (4 GiB) suspends a tree only while the host is not NORMAL — freezing one big workload on a
  healthy host gains nothing, and SIGSTOP does not shrink RSS anyway.
- **Faster memory decision.** The memory limit uses its own 5 s sustain window (the CPU window
  stays 20 s), so a test run that eats gigabytes in seconds is caught before swap is.
- **Automatic resume after a memory crisis.** When host memory returns to NORMAL, workloads
  suspended for memory come back one every 3 s, newest freeze first. Manual suspensions stay
  untouched.
- **Protect is honored by the guard.** Terminals you protected from pressure relief are now
  also excluded from the guard's automatic suspensions (both the per-terminal limit and the
  critical-pressure victim pick).
- **Kernel memory-pressure signal.** The daemon reads the kernel's own crisis verdict every
  tick — macOS `kern.memorystatus_vm_pressure_level` (critical only; the routine warn level is
  ignored) and Linux PSI (`some avg10 > 20` or `full avg10 > 5`) — and goes CRITICAL
  immediately while the kernel reports one.
- **No more false CRITICAL on large-RAM hosts.** The "< 10 % available" rule is gone; it
  rejected new sessions with more than 1 GiB still free while macOS was comfortably
  compressing. Memory is now CRITICAL below 1 GiB available or on the kernel signal, WARNING
  below 12 %, and recovers at max(10 %, host reserve) for 10 s.
- **Installed daemons use the documented thresholds.** `defaults.json` is embedded in the
  binary; 0.1.0 read it from the build machine's path and fell back to drifted values on
  every other machine.

### Sessions & workspace

- **Agent conversations resume on relaunch, paced.** Restored panes whose agent conversation
  had ended resume it in place — at most three at a time, and waiting (up to 30 s) while host
  memory is CRITICAL — without holding up the rest of the restore. Duplicate panes that point
  at the same conversation keep the Resume button instead.
- **Closing a tab's last pane closes the tab**; freshly created empty tabs stay.
- **Single instance per data directory.** Launching the app again brings the running window to
  the front instead of racing it for the daemon.

### Interface

- The resource strip shows the AI agents detected in your terminals per kind, with how many
  are working or waiting for you; managed-run counts appear only when there are any.
- The "outdated daemon" banner is gone — it kept shifting the layout, and the daemon copy is
  refreshed on launch anyway.

### Upgrading from 0.1.0

Terminals you keep running in the background stay attached to the daemon that started them.
To pick up the new resource governance right away, quit IYAGI Term and choose **not** to keep
terminals running before installing 0.1.1. The old daemon exits on its own after five minutes
with no terminals and no app connected; the next launch starts the 0.1.1 daemon, and ended
agent conversations can be resumed from the pane or the recent sessions dialog.

| Platform | File | SHA-256 |
|---|---|---|
| macOS · Apple Silicon | [`IYAGI.Term_0.1.1_aarch64.dmg`](releases/IYAGI.Term_0.1.1_aarch64.dmg) | `aed7aab63a89d203f215edb01a44bf6321494807453a7daea531534647535186` |

Signed with Developer ID and notarized; the ticket is stapled to the DMG.

## 0.1.0 — 2026-10-09

First public release: **R1 — local terminal + managed execution.**

- Terminal workbench: tabs and nested splits (8 per tab), tab regrouping and the layout editor,
  command palette, rebindable shortcuts, macOS menu bar, terminal context menu, safe
  clipboard and links, workspace restore with history replay
- Execution-manager daemon (`iyagi-termd`) that owns the PTYs, so terminals outlive the window
  and the app
- Admission control with an aging priority queue, OS-level process groups, resource guard and
  pressure relief, per-tree resource attribution and 5-minute graphs
- AI agent awareness for Claude Code, Codex and OpenCode — header metadata, working/waiting
  markers, resume, notification center and subscription usage gauges; optional Z.ai (GLM)
  routing for Claude Code terminals
- Korean IME bridge for macOS WKWebView × xterm 6, `Shift+Space` 한/영 toggle, bundled Korean
  coding font
- AI missions engine and UI behind a development-build gate (off in release builds)

| Platform | File | SHA-256 |
|---|---|---|
| macOS · Apple Silicon | [`IYAGI.Term_0.1.0_aarch64.dmg`](releases/IYAGI.Term_0.1.0_aarch64.dmg) | `352c1f365887a05128cbe164b091e967fa665491bba5db2cdd4c1ba5c081d72e` |

Signed with Developer ID and notarized; the ticket is stapled to the DMG.
