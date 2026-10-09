# How AI missions work

In the app, **Settings → AI missions → How it works** and the bottom of the **New AI mission** dialog show the same basic flow.
AI missions are a development-build preview. Operational and internal details — authentication setup, recovery and reconciliation, verification isolation, automatic integration — are in the [details document](USER_GUIDE_DETAILS.md); release evidence and remaining limitations are in [implementation status](IMPLEMENTATION_STATUS.md).

## Quick start

1. **Prepare** — Install your coding CLI and sign in with the official CLI (`codex login` for Codex; run `claude` and use `/login` for Claude Code). In **Settings → AI missions → Quick setup**, confirm the model on an installed CLI's card and click **Create team with this model**. The combination currently verified for all four roles is Codex 0.154.0 on Apple Silicon macOS with ChatGPT subscription and `gpt-5.6-luna`.
2. **Start** — Right-click a terminal pane in the repository folder and choose **New AI mission here…**, or click **New AI mission** in the top bar (`Cmd/Ctrl+Shift+M`). Write the goal and click **Create and start**. When you have uncommitted changes, **Start with the uncommitted changes included** is on by default, so the mission starts from the working tree you are looking at. The app never commits or stashes anything for you. Clear the box to start from the current commit (HEAD) instead, which then requires a clean worktree.
3. **Progress** — The Lead splits the goal into tasks and Builders work in their own copies (workspaces). You are asked only when needed, under **Decision needed**.
4. **Check** — The combined result is checked with your repository's verification commands and an independent review (quick mode skips the review).
5. **Accept** — Look over the changes in **Result**, click **Accept this result**, then use the commands under **Bring the result in** to apply it to your branch.

OpenCode needs a saved connection, so Quick setup cannot create it. Follow [Manual configuration](USER_GUIDE_DETAILS.md#manual-configuration) and [Saved OpenCode connections](USER_GUIDE_DETAILS.md#saved-opencode-connections).

## What a mission does and when to use one

A mission hands one goal to an AI team. The **Lead** splits the goal into tasks, **Builders** implement each task in a separate daemon-owned Git workspace, and an automatic integration step combines the changes. The combined result runs your verification commands, the **Reviewer** reviews it independently, and you accept it. The **Integrator** handles conflicts. Verification failures and major review findings lead to repair plans within your limits.

| | Running `claude` or `codex` in a terminal | AI mission |
| --- | --- | --- |
| Where work happens | Files change directly in your checkout | Agents work in daemon-owned copies; your branch changes only when you bring the accepted result in |
| How it runs | You steer one agent in a conversation | Give a goal and completion requirements; several tasks run in parallel and you are asked only when needed |
| Checking | You test and read yourself | Every result goes through verification commands, independent review, and an acceptance checklist |
| Disconnects and restarts | Reopen the session and continue | Run records and workspaces are kept; runs with unknown outcomes are quarantined until you choose a new attempt |
| Limits | The CLI's own settings | Parallel runs, attempts, active time, and cost cap per mission |

**Use a mission** for work with clear completion requirements that splits into several files or tasks, for repositories with verification commands, or when you want the result as a commit to review.
**Use a terminal** for exploring code, quick questions, interactive debugging, or continuing directly on top of uncommitted changes.

A pane running an agent has **Hand this repository to an AI team…** in its ⋯ menu. It opens a new mission for the same repository, but the current conversation is not carried over, so write the goal again.

## Reading the mission screen

Each mission opens in its own tab. The left side is the **Lead conversation** (your goal, the adopted plan, progress reports) with **Message the whole mission** below. The upper right is the **Tasks** list; the lower right shows either **Run detail** or **Result**. Narrow windows switch between **Conversation / Tasks / Result**.

- **Header**: title, state and phase, active time (used / limit). **Mission controls ▾** has **Pause new runs**, **Resume**, **Stop mission**, **Limits for this mission…**, and **Change the team…**. While pausing or stopping, it shows how many runs are waiting for stop confirmation and **Open the first run**.
- **Decision banner**: `Decision needed N: …` with **Open**. The decision panel shows a one-line situation, the material to decide on (proposed tasks, file changes, conflicting files, affected tasks), option buttons with what each one does, and **Details**. After you answer, it moves to the next open decision and briefly shows “Answer saved”. Decisions never force a tab switch.
- **Awaiting acceptance**: when the phase changes to awaiting acceptance, the lower right switches to Result once (the Result tab on narrow windows). After that your choice is kept. The dot next to Result means acceptance is pending.
- **Tasks list**: each row shows a status glyph and label (Waiting on upstream, Ready, Starting, Running, Needs input, Awaiting review, Blocked, Task done, Failed, Cancelled, Superseded by plan), the model, and attempts. A second line adds the waiting reason, retries left, attempt history, and an **Experimental connection** badge. After three minutes without activity a run shows “checking”; that is not a failure. Counters `Running N · Done N · Decisions N`, filters All / Running / Needs input / Waiting / Done, and search by title, role, or model are at the top.
- **Task details**: role, model, attempt, recorded run time, a one-line status notice (+ **Details**), controls (**Stop this task's run**, **Retry**, **Change model**) with the reason when disabled, and **Send more instructions to this task**. Tabs: Activity (follows new output, stops when you scroll up, **Jump to latest**), and Changes / Verification (items for this task first, from the whole result).
- **Tab badge**: one badge by priority — `Decision needed N` > `Awaiting confirmation` > `Failed` > `Running N` > `Done`. A tab you are not viewing briefly announces when it starts needing a decision.
- **Notification center**: records new decisions, entering awaiting acceptance, and failure, one line each. Nothing is recorded while you are viewing that mission's tab; with the window hidden, decisions and awaiting acceptance also send a desktop notification.
- **Mission list**: open it with the ☰ button in the top bar or from the menus. It groups missions into Needs decision / Awaiting confirmation / In progress / Finished / Archived, searches titles and repositories, and opens a tab when you click a row. Up to 16 mission tabs can be open.
- **Closing tabs and archiving**: closing a tab keeps the mission running; reopen it from the list. **Archive** is available only for finished missions and deletes no files or workspaces. Use **Undo** on the toast right after archiving, or **Unarchive** under Archived in the list.
- **Disconnected**: `Disconnected · run state unknown` dims the screen and locks buttons that change anything. Daemon runs continue, and the screen resyncs automatically after reconnecting. A temporary delay shows `Sync delayed` with **Try again** instead.

## Accepting and bringing in the result

The top of the result screen shows the state, the **Before you accept** checklist, **Accept this result** (with the first blocking reason), **Request changes**, **Discard this result**, and a summary of how to bring the result in. Below are the items to check yourself, the requirements table, changed files, verification with logs, review findings, remaining limitations, the full import section, and run history/usage.

- Checklist items (only those that apply): awaiting acceptance, required tasks done, requirement checks passed on this result, verification input write-blocking enforced, independent review done, no open blocking or major findings, your confirmations checked, no running or unresolved runs, no unanswered blocking decisions. Links next to items jump to the task, check, finding, or decision.
- **Check yourself**: tick human-check requirements, verification without enforced input write-blocking, and past runs with unknown outcomes after checking them. If the result changes after you open the screen, acceptance is disabled and your checks are cleared; **Refresh** and check again.
- **Accepting** cannot be undone. It records the verification and your confirmations and makes this result final, but it does not update your original branch.
- **Bring the result in**: the result is stored as a commit on a private ref in your repository. The screen inspects your current branch to recommend a command; copy it or use **Open in terminal** (opens a terminal tab at the repository and copies the command without running it).
  - Merge into current branch: `git merge --ff-only <result commit>` — succeeds only while your branch is still at the mission's base commit.
  - Bring in on a new branch: `git switch -c ai/<name from title> <result commit>` — recommended when your branch has moved.
  - View the changes: `git diff <base commit> <result commit>`.
- **Follow-up missions**: a completed mission offers **Start a follow-up mission from this result** in place of the composer. If the previous mission was accepted and the repository is the same, the new mission starts on top of the accepted result commit (the dialog shows `Starts on top of the previously accepted result · commit …`). Otherwise it starts from the current HEAD. Failed or stopped missions offer **New mission with the same goal**.
- **Request changes** moves to the Lead composer. **Discard this result** stops the mission and archives it once it has ended (files and workspaces are not deleted).

## Quick mode (skip review)

Choose **Skip review (faster)** under **Review** in the new mission dialog to accept with verification commands and your own confirmation, without a Reviewer. It is the default for teams without a Reviewer; **Include review (recommended)** needs a Reviewer connection. The checklist no longer has an independent-review item, so look over the changes more carefully yourself. Without verification commands, or on an OS where they cannot run, your confirmation is the only acceptance evidence.

## Experimental connections

Each connection carries one trust indicator: **Verified at release** / **Verified on this PC** / **Experimental** / **Unverified**. When a Quick setup card shows **Automated runs unverified** or **Verified for this version line · confirm now to lock it in**, you can consent to **Use experimentally** and create a team, as long as the required features are implemented (**Verified on this PC** needs no consent at all). Result format, cancellation, and the scope of file changes may not behave as expected.

- Consent is per connection, once. A CLI update does not require re-consent — just click **Check now** in settings.
- Some combinations are usable only in quick mode (review skipped).
- The use stays visible: “Includes an experimental connection” in the new mission dialog, an **Experimental connection** badge (shown only when a required capability rests on consent) on task rows and details, and a warning plus an information row in the result checklist. The information row does not block acceptance.

## Cleaning up workspaces

The copy folders (workspaces) made for tasks are not deleted automatically when a mission ends. They are cleaned only when you click.

- The **mission list** header shows total usage (`AI workspaces 1.2 GB`), and Finished and Archived rows show **Clean up workspace (size)** or why cleanup is unavailable.
- Right after acceptance, the result screen suggests **Clean up (size) of workspace**. The archive toast mentions that cleanup is available in the list, with **Open list**.
- The confirmation explains that the mission's copy folders and temporary records are deleted, accepted result commits stay so you can still bring them in, and your original repository is not touched.
- Afterwards it shows how many were removed, the space freed, and kept items with reasons (uncommitted changes, a run still using it, an unconfirmed folder, skipped for time, and so on). Skipped items continue on the next cleanup.
- Missions still in progress, or with a run whose exit is unconfirmed, cannot be cleaned up. A stuck run can be cleared with **I confirmed the process has exited** in task details.

## Troubleshooting

| What you see | What to do | Details |
| --- | --- | --- |
| **Create and start** disabled, “Verified automatic execution support is missing for …” | Create a team with Quick setup or run **Check installation** in settings | [Manual configuration](USER_GUIDE_DETAILS.md#manual-configuration) |
| “Pending Git changes exist” / “The repository has uncommitted changes” | Tick the include box, or commit or stash, then start again | [Start a mission](USER_GUIDE_DETAILS.md#start-a-mission) |
| “The repository has no commits” | Create the first commit and retry | [Start a mission](USER_GUIDE_DETAILS.md#start-a-mission) |
| “The repository HEAD changed after the mission was created” | **Recreate from the current HEAD** | [Start a mission](USER_GUIDE_DETAILS.md#start-a-mission) |
| “Sign-in required” / failure with a sign-in error | Run `codex login`, or `claude` then `/login`, and choose **Retry after fixing the cause** | [Codex](USER_GUIDE_DETAILS.md#codex-authentication) · [Claude](USER_GUIDE_DETAILS.md#claude-authentication) authentication |
| “Waiting: required capability not verified” | Check installation (**Check now**) in settings or **Change model** | [Manual configuration](USER_GUIDE_DETAILS.md#manual-configuration) |
| “Provider rate limit · resets …” | Wait for the reset or **Change model** | [Provider limits](USER_GUIDE_DETAILS.md#when-the-provider-limits-requests) |
| “Automatic retry · after …” | Wait (at most two retries) | [Automatic retry](USER_GUIDE_DETAILS.md#waiting-for-an-automatic-retry) |
| “Waiting for plan format correction” / “Plan auto-correction stopped · decision needed” | Wait, or choose the next step in Decision needed | [Plan format correction](USER_GUIDE_DETAILS.md#waiting-for-a-plan-format-correction) |
| Waiting on the cost limit | **Adjust this mission's limits** in Decision needed, or **Change model** | [Cost admission](USER_GUIDE_DETAILS.md#when-a-task-waits-for-cost-admission) |
| Active time or automatic start limit reached | Raise the limit in **Limits for this mission…** | [Pause, stop, and time](USER_GUIDE_DETAILS.md#pause-stop-and-time) |
| “Integration hit a conflict” | **Ask the integrator to resolve**, or **Exclude the conflicting change and ask the Lead to replan** | [Automatic integration](USER_GUIDE_DETAILS.md#automatic-integration) |
| “Automatic review repair reached its limit” | Dismiss findings with reasons in Result, raise the limit, or stop the mission | [Replacement plans](USER_GUIDE_DETAILS.md#follow-a-required-task-replacement-plan) |
| “Required-task repair plan in progress” / “Lead repair plan failed” | Wait for the plan, or check Decision needed | [Replacement plans](USER_GUIDE_DETAILS.md#follow-a-required-task-replacement-plan) |
| “Execution ended · choose whether to retry” / “Confirming the previous run ended” | Review earlier changes and external effects, then **Start a new attempt after reviewing effects** | [Recover runs with unknown outcomes](USER_GUIDE_DETAILS.md#recover-runs-with-unknown-outcomes) |
| “No termination evidence yet, so this run still holds an execution slot” | Check that the process is gone, then **I confirmed the process has exited** | [Confirm a stuck run yourself](USER_GUIDE_DETAILS.md#confirm-a-stuck-run-yourself) |
| “Delivery unconfirmed” | Check the run record, then **Review and resend** | [Sending and resending messages](USER_GUIDE_DETAILS.md#sending-and-resending-messages) |
| “The save response could not be confirmed” | **Check send result again** | [Sending and resending messages](USER_GUIDE_DETAILS.md#sending-and-resending-messages) |
| “Running verification commands automatically is not supported on this OS yet” | Use human-check requirements | [Verification](USER_GUIDE_DETAILS.md#verification-execution-and-recovery) |
| ff-only merge fails / “Your current branch has moved since the mission started” | Use **Bring in on a new branch** | [Accept the result](USER_GUIDE_DETAILS.md#accept-the-result) |
| “Disconnected · run state unknown” | Wait for the daemon connection (runs continue) | — |
| “The daemon speaks a newer AI mission protocol than this app” | Update the app | [Entry points and availability](USER_GUIDE_DETAILS.md#entry-points-and-availability) |
| Workspace cleanup unavailable | Wait for the mission to end or clear the stuck run, then retry | [How workspace cleanup works](USER_GUIDE_DETAILS.md#how-workspace-cleanup-works) |

## Claude Code terminals → Z.ai (GLM)

This section covers the interactive Claude Code **terminals** that iyagi opens, not AI mission runs. Missions keep using the daemon connection store described in [the details](USER_GUIDE_DETAILS.md#claude-authentication) (`iyagi-termd connection add`); the two stores do not share a key, so register it in both places if you use both.

1. Open **Settings → Integrations → Z.ai Coding Plan** and save the key once. It is encrypted in app-local storage on this device (`<data dir>/secrets/zai.enc`) without a keychain prompt, and the daemon reads the same file at launch time. Saving a key turns **Route Claude Code launches through Z.ai (GLM)** on (only when the connected daemon supports routing; otherwise flip the switch after updating the daemon); deleting the key turns it off. Turning the switch off is always possible, even against an older daemon.
2. Choose the main model: **GLM-5.3** (`glm-5.3[1m]`, default) or **GLM-5.3-Flash** (`glm-5.3-flash[1m]`). The haiku/background slot is always GLM-5.3-Flash. Both controls are disabled with a hint while no key is stored or the running daemon does not advertise routing; in that case the app refuses a routed launch with a message instead of silently falling back to Anthropic.

What is routed: quickstart Claude launches from the empty-project screen, managed-run profiles whose CLI is Claude Code, and **Resume** of a Claude session. Plain shells are never routed, even if you type `claude` in them; such a pane uses whatever your own shell environment provides.

The daemon resolves the key itself and injects the following environment. The key never enters the launch request, the database, the pane title or logs.

| Variable | Value |
| --- | --- |
| `ANTHROPIC_BASE_URL` | `https://api.z.ai/api/anthropic` |
| `ANTHROPIC_AUTH_TOKEN` | your Z.ai key |
| `ANTHROPIC_DEFAULT_OPUS_MODEL`, `ANTHROPIC_DEFAULT_SONNET_MODEL` | the selected main model |
| `ANTHROPIC_DEFAULT_HAIKU_MODEL` | `glm-5.3-flash[1m]` |
| `CLAUDE_CODE_AUTO_COMPACT_WINDOW` | `1000000` |
| `API_TIMEOUT_MS` | `3000000` |
| `CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC` | `1` |
| `CLAUDE_CODE_PROVIDER_MANAGED_BY_HOST` | `1` |

Inherited `ANTHROPIC_API_KEY`, `CLAUDE_CODE_OAUTH_TOKEN`, `CLAUDE_CODE_USE_BEDROCK`/`VERTEX`/`FOUNDRY`/`GATEWAY`, `ANTHROPIC_MODEL`, `ANTHROPIC_DEFAULT_MODEL`, `ANTHROPIC_SMALL_FAST_MODEL` and `CLAUDE_CODE_SUBAGENT_MODEL` are removed from the child before these values apply, so a shell profile that points at another gateway cannot leak through. `CLAUDE_CONFIG_DIR`, `HOME` and temp directories are left alone: your `~/.claude` (hooks, status line, plugins, MCP servers, sessions and resume) runs as usual, and no `--model` argument is added, so `/model` and `settings.json` keep working. A launch profile that already sets one of the authentication or base-URL variables fails with an environment-conflict message rather than being overridden.

A saved claude.ai login stays signed in but unused while routed; Claude Code may print a one-line warning that the token variable takes precedence over the login. That is expected. Features that need Anthropic's servers are unavailable while routed: Remote Control, `/schedule`, claude.ai MCP connectors, MCP tool search and auto-update.

Windows: install Claude Code with the native installer and register its `claude.exe` as the profile executable. The npm `claude.cmd` shim cannot be a managed-run program (see 02 §3), and quickstart already skips shims.

Launch errors and what to do: `zai_key_missing` — register the key under Settings → Z.ai Coding Plan; `daemon_outdated` — the running daemon does not advertise routing, update or restart it; `claude_provider_program_mismatch` — the profile's executable is not Claude Code; `claude_provider_env_conflict` — remove the conflicting variables from the profile. [Z.ai Claude Code guide](https://docs.z.ai/devpack/tool/claude), [Claude Code environment variables](https://code.claude.com/docs/en/env-vars), [LLM gateway configuration](https://code.claude.com/docs/en/llm-gateway-connect)

### ccd / ccg shell commands

Two shell functions start Claude Code straight from a terminal, without an app pane: `ccd` uses Anthropic (your regular claude.ai login), `ccg` routes through Z.ai (GLM) with the key registered above. Both follow whichever main model is currently selected in this section.

Install them from **Settings → Launch profiles → Shell commands ccd / ccg**. Installation writes a 3-line marker block to `~/.zshrc` plus an app-owned script; run `source ~/.zshrc` or open a new terminal to pick them up. If a `ccd` or `ccg` alias or function you defined yourself already exists anywhere in your shell startup files, installation is refused rather than overwriting it.

`ccd` cannot override a Z.ai `env` block already present in `~/.claude/settings.json` — if you previously ran Z.ai's own installer, remove that block by hand, or `ccd` will still talk to Z.ai instead of Anthropic. If you delete the stored Z.ai key, `ccg` prints a message asking you to register one instead of starting.

zsh only (macOS/Linux); Windows is not supported.
