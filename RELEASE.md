# Release notes

Installers are published as [GitHub Releases](https://github.com/sh2orc/iyagi-term/releases),
one per version (tag `v<version>`), and the latest is linked from the [README](README.md#download)
with its SHA-256. How releases are built, signed, notarized and published:
[releases/README.md](releases/README.md).

## 0.1.11 — 2026-11-10

IYAGI Term now trusts the certificates in the macOS keychain as well as its bundled roots, so
its own connections work behind corporate TLS inspection (ePrism and similar) whose CA is
installed on the Mac.

### Networking

- **Keychain certificates are trusted.** The app and the daemon checked HTTPS servers only
  against the Mozilla roots bundled into the binaries and ignored the keychain. Behind a
  TLS-inspection proxy, which re-signs every site with a company CA, all of their own
  connections failed: the Kimi and Z.ai usage gauges, the Kimi login refresh, the gateway's
  upstream calls for Z.ai-routed Claude Code terminals, and the network check that
  auto-continue makes before resuming (it then reported the network as down). Both sets are
  trusted together now. If the keychain cannot be read, the bundled roots still apply, so
  nothing that connected before stops connecting.
- **`SSL_CERT_FILE` / `SSL_CERT_DIR`.** When the app is started with either variable set,
  that bundle or directory is read instead of the keychain, still on top of the bundled roots.
- **Not covered yet: CLIs inside terminals.** Claude Code, Codex, `kimi` and other tools
  running in a pane connect on their own and use their own trust settings. Behind TLS
  inspection they still need the company CA in their environment, for example
  `NODE_EXTRA_CA_CERTS` for Node-based CLIs.

### Upgrading from 0.1.10

Certificates are read when a connection client is created. After installing a new company CA,
restart the app. The gateway and the network check run in the daemon: when Settings › General
offers a restart for the 0.1.11 daemon, press **Restart**. Restarting ends open terminals;
agent conversations can be resumed from each pane.

| Platform | File | SHA-256 |
|---|---|---|
| macOS · Apple Silicon | [`IYAGI.Term_0.1.11_aarch64.dmg`](https://github.com/sh2orc/iyagi-term/releases/download/v0.1.11/IYAGI.Term_0.1.11_aarch64.dmg) | `9ae2d97d389c76a390da8f1e89f0f2343e9f2d223d0b239a1e5ec154681b91da` |

Signed with Developer ID and notarized; the ticket is stapled to the DMG.

## 0.1.10 — 2026-11-10

The Kimi usage gauge keeps reading from the `kimi` CLI login after its short-lived access token
expires. IYAGI Term now refreshes the login itself, under the same lock the CLI uses, instead
of waiting for the CLI to run again.

### Kimi

- **The CLI login no longer goes stale.** 0.1.9 used the `kimi` CLI's access token only while
  it was valid (about 15 minutes after the CLI last refreshed it) and never refreshed it. Now
  an expired token, or one the usage endpoint rejects, is refreshed through the CLI's own OAuth
  endpoint for the account's region (`auth.kimi.ai` or `auth.kimi.com`).
- **Shared with the CLI, not competing with it.** The refresh holds the CLI's
  `oauth/<slot>.lock`, so the app and a running `kimi` never rotate the refresh token at the
  same time; if the CLI has already refreshed, its new token is used as is. The rotated
  credentials are written back to the CLI's credential file atomically and readable only by
  you (`0600`), so the CLI keeps working with them.
- **When the login cannot be refreshed.** If the refresh is refused, for example because the
  login was revoked, the gauge suggests running `/login` in `kimi` again or storing a
  membership key, and the last reading stays on screen with the time it was taken. A
  membership key stored in Settings › Integrations › Kimi is still used when the login is not.
- **Wording.** The credential-store error now says the store could not be read or written,
  since the app also writes the CLI's credential file.

### Upgrading from 0.1.9

The usage gauge runs in the app, so this fix needs no daemon restart. Settings › General may
still offer one because the daemon's version changed; it can wait until a convenient time.

| Platform | File | SHA-256 |
|---|---|---|
| macOS · Apple Silicon | [`IYAGI.Term_0.1.10_aarch64.dmg`](https://github.com/sh2orc/iyagi-term/releases/download/v0.1.10/IYAGI.Term_0.1.10_aarch64.dmg) | `815027080f4054e76f302bbd59479c52320922efe0a66ea058063135790e2086` |

Signed with Developer ID and notarized; the ticket is stapled to the DMG.

## 0.1.9 — 2026-11-09

Kimi now follows the membership alone. The usage gauge reads the quota from the `kimi` CLI
login or a membership key in the account's own region. Pay-as-you-go Open Platform support
and the `cck` launcher are gone, and the Kimi CLI becomes a built-in launch profile.

### Kimi

- **Usage that actually reads.** 0.1.8 asked only the mainland China endpoint
  (`api.kimi.com`) and sent the key without the `Bearer` scheme, so most accounts saw no
  quota. The gauge now uses the `kimi` CLI login while its access token is still valid. That
  use is read-only: the app never refreshes the token, because refreshing rotates the CLI's
  refresh token. Otherwise it uses the membership key stored in Settings › Integrations ›
  Kimi, tried against the global (`api.kimi.ai`) and mainland deployments; whichever accepts
  the key is asked first from then on. When the login token has lapsed and no key is stored,
  the last reading stays on screen with the time it was taken.
- **Clearer failures.** A key that neither deployment accepts is reported as such, with a hint
  that it may be an Open Platform key saved where a membership key goes. An expired login
  suggests running `kimi` or storing a membership key.
- **Open Platform removed.** Kimi Open Platform is billed per use, unlike the membership.
  Its key slot, the platform choice and the balance row are gone, and an Open Platform key
  stored by 0.1.8 is deleted the first time the app refreshes usage.
- **`cck` removed.** Launch profiles install `ccd` and `ccg` again, as before 0.1.8. Run Kimi
  with the `kimi` CLI instead.

### Launch profiles

- **Kimi CLI is a built-in profile.** Next to codex, claude and opencode there is now a `kimi`
  profile. It is filled in automatically when the CLI is found, including its own install
  directory `~/.kimi-code/bin`. Existing installs receive it once. It is skipped if you already
  have a Kimi CLI profile or a profile labelled `kimi`.

### Upgrading from 0.1.8

If you applied the shell commands in 0.1.8, Settings › Launch profiles now shows them as out of
date. Apply them once more to drop `cck`; until then, `cck` only prints how to remove it.

| Platform | File | SHA-256 |
|---|---|---|
| macOS · Apple Silicon | [`IYAGI.Term_0.1.9_aarch64.dmg`](https://github.com/sh2orc/iyagi-term/releases/download/v0.1.9/IYAGI.Term_0.1.9_aarch64.dmg) | `30ed93ab6c4db550db589ed7fbb87fb5ce57361bdba170096fc2e5ea0720f140` |

Signed with Developer ID and notarized; the ticket is stapled to the DMG.

## 0.1.8 — 2026-11-09

Kimi joins the providers IYAGI Term knows about: the Kimi CLI is detected like the other
agents, Kimi Code membership quota and Open Platform balance appear in the usage popover, and
a third shell command, `cck`, starts Claude Code against the Kimi platform and model you pick.

### Kimi integration

- **Two platforms, two key slots.** Settings › Integrations › Kimi stores a Kimi Code
  membership key and a Kimi Open Platform key in separate encrypted slots (`term-secrets`,
  like the Z.ai key). The two platforms do not accept each other's keys, so replacing or
  deleting one never touches the other.
- **`cck` starts Claude Code on Kimi.** Launch profiles now install `ccd`, `ccg` and `cck`.
  `cck` uses the platform and main model selected in the Kimi card: Kimi Code offers
  `k3[1m]`, `k3.1-pro[1m]` and `k3.1-pro` (256K); Open Platform offers `kimi-k3[1m]` and
  `kimi-k3.1-pro[1m]`. Context limits and every task tier follow the selected model, and
  inherited provider keys, backend switches and model overrides are cleared before launch.
  The key is read from the encrypted store at launch and never written to a shell file. If the
  selected platform has no key, `cck` says so instead of starting. Re-apply the launch profile
  after changing the platform or model.
- **Usage and balance.** The status bar's usage popover shows Kimi Code's 5-hour, weekly and
  monthly allowance and the Open Platform's available balance (cash and voucher).
- **Kimi CLI awareness.** The daemon detects the `kimi` CLI in any pane by its process, like
  Claude Code, Codex and OpenCode. The pane header shows its model, effort and — new for every
  agent that supports several backends — the provider it is actually using (Kimi Code, Kimi
  Open Platform or a custom provider), read from `config.toml` or an inline `--config`.

### Upgrading from 0.1.7

`cck` works as soon as the app has started once (it refreshes the daemon copy the shell
command runs). Detecting the Kimi CLI and showing providers in pane headers need the 0.1.8
daemon: Settings › General will say a newer daemon is installed — press **Restart**.
Restarting ends open terminals; agent conversations can be resumed from each pane.

| Platform | File | SHA-256 |
|---|---|---|
| macOS · Apple Silicon | [`IYAGI.Term_0.1.8_aarch64.dmg`](https://github.com/sh2orc/iyagi-term/releases/download/v0.1.8/IYAGI.Term_0.1.8_aarch64.dmg) | `e346f546092c4bc4a506d429776a63177f28b1c3b90fe4a1f8038797785becb1` |

Signed with Developer ID and notarized; the ticket is stapled to the DMG.

## 0.1.7 — 2026-11-07

A small renderer release: long sessions in the WebGL renderer no longer pick up stray glyph
fragments after the glyph atlas is cleared, and a frame that cannot settle on a stable atlas
hands the pane to the DOM renderer instead of painting stale coordinates.

### Terminal

- **Clearing the glyph atlas clears every page.** The vendored WebGL addon decided that the
  atlas was already empty by looking only at the first page's write cursor. After a page merge
  that cursor can sit at the origin while other pages still hold glyphs, so a clear could be
  skipped and those glyphs later drawn from stale page numbers — fragments of other glyphs in
  long, colourful TUI sessions. Every page is now checked, and each page's glyph list and
  usage are reset together with its cursor.
- **Invisible cells restore the canvas state.** Rasterizing an invisible cell returned before
  restoring the 2D context state it had saved, so the save stack grew with every such cell.
  That path now restores it too.
- **An unstable frame falls back instead of painting stale coordinates.** When the atlas page
  layout keeps changing while one frame is built and the retry limit (32) is reached, the
  renderer used to draw anyway with coordinates that could point at the wrong page. It now
  retires that renderer through the existing context-loss path: the pane switches to the DOM
  renderer and WebGL is recreated shortly after.

### Tests

- Daemon integration tests no longer fail where the platform or a slow runner, not the code,
  is the cause: the Codex account-validation test waits (up to 5 s) for the run to finish
  before asserting; relief specs skip on hosts without scheduling-yield support; mission e2e
  scenarios that reach verification acceptance run only on macOS, where verification isolation
  exists; and the shell input-completion fixture stays alive with `echo`.

### Upgrading from 0.1.6

The daemon is unchanged apart from its version number. Settings › General will say a newer
daemon is installed; restarting it is optional. Restarting ends open terminals; agent
conversations can be resumed from each pane.

| Platform | File | SHA-256 |
|---|---|---|
| macOS · Apple Silicon | [`IYAGI.Term_0.1.7_aarch64.dmg`](https://github.com/sh2orc/iyagi-term/releases/download/v0.1.7/IYAGI.Term_0.1.7_aarch64.dmg) | `6d333024f5a6d772394d743c012fa720742f57e77fe76f1405a175dee0df8243` |

Signed with Developer ID and notarized; the ticket is stapled to the DMG.

## 0.1.6 — 2026-10-28

A small interface release: the tab bar keeps the group you switched to in view, and mission
test suites no longer paint Linux CI red where the platform simply does not support them.

### Terminal

- **The tab bar reveals the group you swipe to.** A two-finger swipe can land on a group whose
  tab sits outside the visible part of the tab bar — the bar used to stay put, so repeated
  swipes in one direction ended on an active group you could not see. The bar now scrolls the
  minimum needed to bring it in (6 px of breathing room), measured in the bar's own content
  coordinates so the reveal lands exactly where intended. A group that is already visible is
  never nudged — a position you pushed with the trackpad is left alone — and changes that
  happen while the workbench is hidden (settings, layout editor) can no longer reset it.

### Tests

- The mission verification/integration exec suites now compile only on macOS, where the
  sandbox-exec isolation they exercise exists. On Linux CI they used to show as failures — a
  platform boundary, not a regression. Their shared `ExecSupervisor` fixture moved to
  `exec_supervisor.rs` so every OS can still use it.

### Upgrading from 0.1.5

The daemon is unchanged apart from its version number. Settings › General will say a newer
daemon is installed; restarting it is optional. Restarting ends open terminals; agent
conversations can be resumed from each pane.

| Platform | File | SHA-256 |
|---|---|---|
| macOS · Apple Silicon | [`IYAGI.Term_0.1.6_aarch64.dmg`](https://github.com/sh2orc/iyagi-term/releases/download/v0.1.6/IYAGI.Term_0.1.6_aarch64.dmg) | `1ae3f0fa2efe29f7ab45fabe2fb3e16a2715011c676802b562088c38dde1e413` |

Signed with Developer ID and notarized; the ticket is stapled to the DMG.

## 0.1.5 — 2026-10-27

Terminals, agents and the daemon itself become a governed resource: memory pressure freezes
work in stages and revives it automatically, CPU monopolists are made to yield, and process
accounting is native and cheap. Also: crisp glyphs on fractional-DPR displays (Windows
125 %/150 %), quota reset times that match provider dashboards, and CI jobs that can no longer
hang for a day.

### Resource governance

- **Memory pressure freezes work in stages.** When available memory runs low the daemon
  freezes terminal workloads step by step — heaviest contributors with recent growth first —
  and starts them again on its own as headroom returns (one every three seconds, with room for
  what they had resident). The pane you are watching is frozen last and revived first.
- **CPU monopolists yield.** A workload that sustains more than 80 % of a core group on an
  otherwise busy host is demoted until it drops under half of that for 30 seconds; a repeat
  monopolist waits twice as long each time.
- **Host-sized guard defaults.** Admission limits no longer assume a server: CPU slots are
  logical cores ÷ 2 and the managed memory budget is a quarter of RAM (clamped to 2–8 GiB).
- **Native process accounting.** Process enumeration and per-process metering moved off
  sysinfo's expensive macOS path (which read every process's argv and env twice per sweep and
  spent more than half the daemon's CPU in kernel time). Memory now counts per-pid footprint —
  the number Activity Monitor shows — instead of summing shared pages twice.
- **Cheaper terminal plumbing.** Progress fsyncs are gone, the PTY pump turns only while a view
  exists, the global journal budget yields fair shares under pressure, and logs are kept for
  two weeks. Under memory pressure the WebGL renderer is reclaimed and snapshot re-requests
  back off.
- **Agent starts queue up.** Restores, "open again" and resume-all start at most three agents
  at a time (one under pressure), 1.5 s apart.

### Terminal

- **Crisp glyphs on fractional DPI.** On Windows scaling of 125 %/150 % the glyph atlas baked
  fonts at fractional pixel sizes (13 px × 1.25 = 16.25 px) with no integer hinting, and the
  CSS canvas size was rounded away from the cell-multiple backing store — together, text
  looked faintly smeared. The atlas now bakes at integer device pixels and the CSS size is the
  exact quotient, so every glyph quad lands on whole pixels. Integer-DPR screens (macOS
  retina) render exactly as before.

### Usage

- **Quota reset times match provider dashboards**, rounded up to the minute.

### Upgrading from 0.1.4

The daemon changes substantially in this release — restart it after upgrading (Settings ›
General shows when a newer daemon is installed). Restarting ends open terminals; agent
conversations can be resumed from each pane.

| Platform | File | SHA-256 |
|---|---|---|
| macOS · Apple Silicon | [`IYAGI.Term_0.1.5_aarch64.dmg`](https://github.com/sh2orc/iyagi-term/releases/download/v0.1.5/IYAGI.Term_0.1.5_aarch64.dmg) | `b719fd230848cb689d059e15cb5ff13d4aae8b5dcf9a2367f06b1eac0b449cc1` |

Signed with Developer ID and notarized; the ticket is stapled to the DMG.

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
