# Release notes

Installers are published as [GitHub Releases](https://github.com/sh2orc/iyagi-term/releases),
one per version (tag `v<version>`), and the latest is linked from the [README](README.md#download)
with its SHA-256. How releases are built, signed, notarized and published:
[releases/README.md](releases/README.md).

## 0.1.4 — 2026-10-16

A maintenance release: held letter keys repeat on macOS.

### Input

- **Held letter keys repeat on macOS.** Holding a letter key moved the cursor once and stopped —
  `h`/`j`/`k`/`l` in vim, for example — while the arrow keys kept repeating. Terminal input goes
  through the web view's hidden text field, so macOS treated a held letter as the start of its
  press-and-hold accent menu instead of repeating it. The app now turns press and hold off for
  itself at launch, so held keys repeat at the system key-repeat rate. Nothing is written to your
  preferences, and an explicit `ApplePressAndHoldEnabled` setting — for the app
  (`defaults write ai.iyagi.term …`) or global — still takes precedence.

### Upgrading from 0.1.3

The daemon is unchanged apart from its version number. Settings › General will say a newer
daemon is installed; restarting it is optional. Restarting ends open terminals; agent
conversations can be resumed from each pane.

| Platform | File | SHA-256 |
|---|---|---|
| macOS · Apple Silicon | [`IYAGI.Term_0.1.4_aarch64.dmg`](https://github.com/sh2orc/iyagi-term/releases/download/v0.1.4/IYAGI.Term_0.1.4_aarch64.dmg) | `ea644c56831fb3f6d99fc77d3b33af1fefecba3f9e3312692870a5878b93c4b1` |

Signed with Developer ID and notarized; the ticket is stapled to the DMG.

## 0.1.3 — 2026-10-13

Model traffic becomes a governed resource: a token usage observatory with a kill switch and an
API key pool for Z.ai-routed Claude Code terminals. Also CJK-wide `Shift+Space`, project-named
regrouping, a WebGL renderer fix — and releases move to GitHub Releases.

### Model gateway (Z.ai routing)

- **Token usage observatory.** Settings › Integrations has a "Token usage observatory" card.
  When it is on, newly started Z.ai-routed Claude Code terminals send their model requests
  through a loopback gateway inside the daemon (`127.0.0.1`, ephemeral port). The gateway keeps
  a per-session token ledger (input, output, cache read/write, requests, failures) and passes
  everything else through unchanged. Each terminal gets a one-time gateway token; the real API
  key never reaches the terminal and is re-read by the daemon for every request. Request and
  response bodies are never stored. Off by default.
- **Kill switch.** One button refuses every request that goes through the gateway (HTTP 403)
  until you lift it. It covers only terminals wrapped by the gateway — ones started before the
  observatory was turned on keep their direct connection.
- **API key pool.** Register several Z.ai keys in the "API key pool" card. The gateway binds a
  key to each session (keys never change mid-conversation) and rotates across sessions
  round-robin or fill-first. A key that hits a rate limit (429/402) rests for a cooldown while
  the same request is retried once on the next key, before any response bytes stream; a key
  that fails authentication (401/403) is quarantined until you register it again. Keys are
  stored encrypted like the single Z.ai key (`term-secrets`, AES-256-GCM).
- The gateway policy (on/off, blocked, pool strategy) is saved with your preferences and
  re-applied whenever the app attaches to the daemon, so it survives daemon restarts.

### Input

- **`Shift+Space` for every CJK input source.** The forced input-source toggle used to need a
  Korean input source; with only Chinese or Japanese enabled it stayed a plain space. It now
  toggles the enabled Korean, Chinese or Japanese input source against Latin, and the balloon
  shows 한/중/일 or A. The Korean-only hold and jamo restoration stay limited to Korean input.

### Terminal

- **Regroup names tabs after their project.** Regrouping by repository names every project
  tab after its folder, appending `-2`, `-3`… when two projects share a name. Tabs you renamed
  yourself keep their names.
- **WebGL atlas fix actually ships.** A repository-only fix to the vendored WebGL addon had
  been missing from the committed bundle, so every resize threw away and redrew the glyph
  atlas. The bundle is rebuilt, reproducible, and CI now fails when it drifts from source.
- **No renderer flapping.** A renderer that loses its GPU context right after creation no
  longer resets the retry budget, so a GPU that keeps losing contexts stops looping between
  WebGL and DOM (each switch had resized the PTY and redrawn TUIs). The budget is refunded only
  after a renderer survives 60 s.

### Build & CI

- The Linux cgroup backend and the daemon compile on Linux again, and the CI gates for
  Linux, macOS and Windows (rustfmt and clippy toolchains, daemon sidecar staging) were fixed so
  they can run to completion.

### Distribution

- Installers are now **GitHub Release assets** instead of files committed under `releases/`.
  Earlier DMGs (0.1.0 – 0.1.2) are attached to their own releases with unchanged hashes.

### Upgrading from 0.1.x

The gateway needs the 0.1.3 daemon. After installing, open Settings › General; if the daemon
row says a newer version is installed, press **Restart**. Restarting ends open terminals; agent
conversations can be resumed from each pane.

| Platform | File | SHA-256 |
|---|---|---|
| macOS · Apple Silicon | [`IYAGI.Term_0.1.3_aarch64.dmg`](https://github.com/sh2orc/iyagi-term/releases/download/v0.1.3/IYAGI.Term_0.1.3_aarch64.dmg) | `fd80ffcf14cd470b919e26cc42fe5f3f321dedf155b01eb5d873ae193da6d8f3` |

Signed with Developer ID and notarized; the ticket is stapled to the DMG.

## 0.1.2 — 2026-10-11

A maintenance release: a way to replace a daemon left over from an older version, and a Korean
IME fix.

### Daemon

- **Daemon restart button.** Settings › General has a "Background run manager (daemon)" row. It
  says whether the running daemon is older than the installed one and restarts it after a
  confirmation. The daemon outlives the app, so updating or relaunching the app could leave the
  old daemon running — for example a 0.1.0 daemon that kept refusing new terminals with "host
  memory pressure is CRITICAL; retry shortly" under thresholds that 0.1.1 had already removed.
  Removing the outdated-daemon banner in 0.1.1 had left no UI path to replace it, because the
  launch-time refresh only renews the daemon binary copy and never the running process.
  Restarting ends open terminals; agent conversations can be resumed from each pane.

### Korean input

- **No NBSP from native composition.** Text that xterm's own composition helper commits late
  (deferred commits, keyCode 229 textarea diffs) is now normalized like the fallback path, so a
  non-breaking space from the hidden textarea no longer reaches the shell as part of a command.
  Clipboard paste and terminal responses keep their original text.

### Upgrading from 0.1.0 or 0.1.1

After installing, open Settings › General. If the daemon row says a newer version is installed,
press **Restart** to switch to the 0.1.2 daemon. Relaunching the app alone does not replace a
daemon that is still running.

| Platform | File | SHA-256 |
|---|---|---|
| macOS · Apple Silicon | [`IYAGI.Term_0.1.2_aarch64.dmg`](https://github.com/sh2orc/iyagi-term/releases/download/v0.1.2/IYAGI.Term_0.1.2_aarch64.dmg) | `c4087d3768e1709bda58aa9b024c4d47e9aaca71fef0768d189b50dc4b31a096` |

Signed with Developer ID and notarized; the ticket is stapled to the DMG.

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
| macOS · Apple Silicon | [`IYAGI.Term_0.1.1_aarch64.dmg`](https://github.com/sh2orc/iyagi-term/releases/download/v0.1.1/IYAGI.Term_0.1.1_aarch64.dmg) | `aed7aab63a89d203f215edb01a44bf6321494807453a7daea531534647535186` |

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
| macOS · Apple Silicon | [`IYAGI.Term_0.1.0_aarch64.dmg`](https://github.com/sh2orc/iyagi-term/releases/download/v0.1.0/IYAGI.Term_0.1.0_aarch64.dmg) | `352c1f365887a05128cbe164b091e967fa665491bba5db2cdd4c1ba5c081d72e` |

Signed with Developer ID and notarized; the ticket is stapled to the DMG.
