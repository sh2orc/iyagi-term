# AI mission details

The short guide is [How AI missions work](USER_GUIDE.md). This document collects operational and internal details: authentication setup, recovery and reconciliation, message receipts, verification isolation, automatic integration, and where things are implemented.
AI missions are a development-build feature. See [implementation status](IMPLEMENTATION_STATUS.md) for release evidence and remaining limitations.

Terms: a **mission** is the whole job for one goal, a **task** is a unit the Lead splits out, a **run** is one attempt at a task, a **result** is the combined change commit (called a candidate in the contracts), and **accepting** records a result as final.

## Quick setup behavior

**Settings → AI missions → Quick setup** finds installed Codex, Claude Code, and OpenCode executables and shows a card for each with the version, executable path, whether a login record exists, and whether automatic execution is verified for this OS and version. Detection saves nothing and never reads the contents of credential files.

A card's model field offers the candidates that CLI advertises. Codex is asked directly; Claude Code has no listing command, so the aliases `--model` accepts are offered alongside the model ids its local records show in use. OpenCode is not asked during detection because Quick setup cannot create it — its candidates arrive from **Check installation** once a saved connection exists. Candidates are a hint: an id the list does not carry can still be typed in, and a listing that fails never fails detection.

Confirm the model on a card and click **Create team with this model**. This saves the model connection, checks the installation, and saves a team template that assigns the same model to all four roles (Lead, Builder, Reviewer, Integrator). Existing matching connections and teams are reused instead of duplicated. If any role is not verified for automatic execution after the installation check, only the connection is saved; no team is created and the reason is shown. To use an unverified version with your consent, see [Experimental connections](USER_GUIDE.md#experimental-connections).

OpenCode needs saved connection references, so Quick setup cannot create it. Follow **Manual configuration** below and [Saved OpenCode connections](#saved-opencode-connections).

## Entry points and availability

The **New AI mission** button in the top bar (`Cmd/Ctrl+Shift+M`), the command palette, the menu bar, **New AI mission here…** in a terminal's right-click menu, and **Hand this repository to an AI team…** in the ⋯ menu of a pane running an agent all open the same dialog. The focused pane's Git top level (or its current folder) fills the repository path. Opening it from an agent pane does not carry over the current conversation.

When the daemon does not advertise `mission_protocol` in its capabilities, development builds disable the entry points with a reason and production builds hide them. When the daemon's protocol revision is newer than the app's, the entry points are disabled with an update-the-app notice regardless of build type.

## Manual configuration

To set every value yourself, open **Manual entry (advanced)** under **Settings → AI missions**.

1. Install your coding CLI. For Codex and Claude choose the subscription login or saved connection described below; for OpenCode register a saved connection. Check [implementation status](IMPLEMENTATION_STATUS.md) for other routes.
2. Under **Settings → AI missions → Model connections → Manual entry (advanced)**, enter a label, runtime, full executable path, provider ID, model ID, and authentication route. The **Model ID** field offers the candidates the installed CLI actually advertises; they are a hint, so an id the list does not carry can still be typed in. When the chosen model advertises reasoning efforts, a **Reasoning effort** field appears next to it; leaving it blank uses the CLI's own default. When you can estimate a run's cost, enter a positive USD amount in **Estimated cost per run**; a blank value means unknown. Save the model.
3. Save the model before using **Check installation** (labeled **Check now**); save any edits first. The check time, version, and entrypoint file fingerprint are stored; a failed check clears the previous evidence. Notices distinguish a missing executable, execution failure, the three-second timeout, excessive output, and an unrecognized version. Your enabled setting is preserved. This checks the configured file, runs local `--version`, and then runs a local, no-inference self-check of the protocol and sandbox boundary on this machine (no model calls or auth tokens; it still does not check login, authentication permissions, or paid inference). A successful check also collects the candidate models for that connection's provider and offers them in the **Model ID** field; that list is a picker hint which never changes the compatibility grade or the roles a connection may take, and a listing that fails leaves the check itself unaffected.
4. Save a **Team template** with explicit Lead, Builder, Reviewer, and Integrator models. Integrator resolves conflicts and defaults to the Builder model. Saved templates display each assignment. Replace older three-role templates with a saved four-role template before creating a new mission. One model can fill several roles; its runs and workspaces remain separate. **Same model for all roles** fills all four roles at once.
5. To run automated checks, inspect the repository under **Repository verification commands** and save a command. Enter the executable separately from its arguments. Arguments can be space-separated (quotes group a single argument) or a JSON array, for example `npm` with `test -- --run` or `["test", "--", "--run"]`. A blank working directory means the repository root.

Connections saved by older development builds without a daemon observation or file fingerprint appear unverified. Run **Check installation** again for that connection. Editing capability flags in settings does not establish compatibility.

Each model needs verified structured results, events, cancellation, and the read or scoped-write capability required by its role. The creation screen identifies unsupported roles and disables **Create and start**. **The verified combination available for all four roles is Codex 0.154.0 on Apple Silicon macOS, with ChatGPT subscription authentication and `gpt-5.6-luna`.** Use provider `openai` and leave both saved references blank. Save this connection, run **Check installation**, and select it for each role. Review paths, changes, and any additional root permission in the decision panel before approving a file change. Approval is disabled when complete changes are unavailable; deny the request or stop the run. This evidence does not cover other versions, models, or authentication routes. This combination passed a real-model mission through planning, two Builders, integration, verification, independent review, and acceptance. [Full mission evidence](CODEX_MACOS_MISSION_01540.md). Other combinations and release validation remain separate work. [Evidence and scope](CODEX_MACOS_01540.md)

A task that shows **Waiting: required capability not verified** consumes no new attempt or budget reservation. Check its installation in settings or choose a compatible connection through **Change model** in task details. A running mission becomes eligible for scheduling once the requirements are met. If the configured file content, resolved path, or CLI version changes, or verification fails after reservation, the run fails before sending the request to the model. Check installation and explicitly retry; the reserved attempt and its history remain recorded.

Linked worktrees, subdirectories, and symlinks into the same Git repository share its repository ID and verification commands. The working path and base HEAD still come from the selected worktree. If several legacy IDs resolve to the same repository, registration reports an error and preserves existing mission and command references.

## Codex authentication

For **ChatGPT subscription**, sign in with official Codex CLI, then save a model connection with runtime **Codex**, provider `openai`, and authentication **Subscription**. The daemon and CLI must use the same `CODEX_HOME` (default: `.codex` in your home directory). Codex manages login and token refresh. The daemon requires a ChatGPT account; an API key login cannot satisfy a subscription binding. [Official authentication guide](https://learn.chatgpt.com/docs/auth)

For an **API key**, register a Codex-specific connection in the OS credential store. The OpenCode section below explains CLI builds, data directories, and stdin:

```sh
pbpaste | ./target/debug/iyagi-termd connection add --preset codex-api --key-stdin
```

Enter both returned references under **Model connections → Codex → API key → Saved credential reference / Saved endpoint ID**. Use provider `openai`; its destination is fixed to `https://api.openai.com/v1`. An OpenCode `openai-api` connection cannot be reused. Rotation and revocation use the common connection commands in the OpenCode section.

API runs use private HOME/CODEX_HOME directories and an ephemeral credential store. The daemon verifies effective configuration before sending the key through official `account/login/start`. Keys are absent from process arguments, environment variables, and launch manifests. Authentication or thread-provider mismatches stop the run. No `auth.json` is written; the private directory is released after process cleanup and durable exit persistence. Subscription runs use the existing Codex-managed home, with a different isolation scope. [App Server authentication](https://learn.chatgpt.com/docs/app-server), [credential-store configuration](https://learn.chatgpt.com/docs/config-file/config-reference)

Codex 0.154.0 API authentication was tested through configuration and authentication metadata. The Apple Silicon macOS subscription combination above also passed real structured output, approved file creation, immediate cancellation, and process cleanup. Resume, active-turn steering, and usage remain unverified on macOS. Other versions fail closed when required configuration or responses cannot be verified. Custom providers/endpoints and local authentication are not supported yet.

## Claude authentication

For an **existing Claude subscription login**, sign in with the official CLI, choose runtime **Claude Code**, provider `anthropic`, and authentication **Subscription**. **Leave both saved references blank.** The daemon and CLI must share the same user home and `CLAUDE_CONFIG_DIR` (default: `~/.claude`). Before sending the request, the daemon checks the running process's initialization response for subscription account metadata. Console API logins and unrecognized authentication are rejected. The official CLI owns login and refresh. [Authentication guide](https://code.claude.com/docs/en/authentication)

For **API keys, separate subscription tokens, or Z.ai Coding**, register an OS credential-store connection. See the OpenCode section below for CLI builds and data-directory selection.

| Preset | Provider ID | Authentication | Destination |
| --- | --- | --- | --- |
| `claude-api` | `anthropic` | API key | `https://api.anthropic.com` |
| `claude-subscription` | `anthropic` | Subscription | `https://api.anthropic.com` |
| `claude-zai-coding` | `zai-coding-plan` | Subscription | `https://api.z.ai/api/anthropic` |

```sh
pbpaste | ./target/debug/iyagi-termd connection add --preset claude-api --key-stdin
```

Enter the returned `credential_ref` and `endpoint_ref` in the Claude model connection, matching the provider and authentication above. To use a separate subscription token, obtain it through official `claude setup-token` and register it with `claude-subscription`. The daemon does not refresh this token: on expiry, create a connection with a new token and replace both references. Specify the exact model ID; consult [Z.ai's integration guide](https://docs.z.ai/devpack/tool/claude) for current model and plan support. OpenCode connections cannot be reused. [Token environment variable](https://code.claude.com/docs/en/env-vars) The key saved under **Settings → Integrations → Z.ai Coding Plan** is a separate store used only for interactive Claude terminals; see [Claude Code terminals → Z.ai (GLM)](USER_GUIDE.md#claude-code-terminals--zai-glm).

Saved connections use private HOME/config/temp directories. Credentials enter only the child's environment, never arguments, mission RPC, or launch manifests. The daemon checks authentication source and permission mode before sending a prompt, and releases the directory after confirmed process cleanup and durable exit persistence. Existing subscription logins share the user's CLI home.

Current Claude mission runs allow `Read/Glob/Grep`, adding `Edit/Write` for write tasks. This path does not serve Bash, user hooks/plugins/MCP, or interactive approvals. Sessions are neither persisted nor resumed; CLI restrictions do not establish OS sandbox enforcement. Installed Claude Code 2.1.271 initialized all three saved authentication routes with dummy credentials. Real subscription login, model access, and paid inference compatibility still need separate evidence. [CLI options](https://code.claude.com/docs/en/cli-reference)

## Saved OpenCode connections

Automated OpenCode runs use a daemon-owned connection, separate from global CLI authentication. Build the development CLI with `cargo build -p iyagi-termd`. Use the same `--data-dir` as the app if it uses a custom directory. Supply a key on stdin from your secret manager; do not put it in an argument or shell command. For example, on macOS with a key already copied:

```sh
pbpaste | ./target/debug/iyagi-termd connection add --preset zai-coding --key-stdin
./target/debug/iyagi-termd connection list
```

| Preset | Provider ID | Authentication | Endpoint |
| --- | --- | --- | --- |
| `zai-coding` | `zai-coding-plan` | Subscription | `https://api.z.ai/api/coding/paas/v4` |
| `zai-api` | `zai` | API key | `https://api.z.ai/api/paas/v4` |
| `openai-api` | `openai` | API key | `https://api.openai.com/v1` |
| `anthropic-api` | `anthropic` | API key | `https://api.anthropic.com/v1` |

Copy the returned `credential_ref` and `endpoint_ref` into **Model connections → OpenCode → Saved credential reference / Saved endpoint ID**. Match the provider and authentication route above; enter the exact API model ID. Keys stay in the OS credential store and are absent from command output, endpoint metadata, and mission RPC.

To rotate, create a new connection and update both binding references. `iyagi-termd connection revoke --endpoint UUID` deletes its stored key while retaining endpoint metadata for existing snapshots. A running child already has its key: stop its run first, and revoke the provider's key separately when required.

The daemon validates the destination and checks OpenCode's effective credentials/endpoint before creating a session. Each run gets its own HOME/XDG/temp directories and explicit environment; confirmed cleanup releases its directory. The [Z.ai integration](https://docs.z.ai/devpack/tool/opencode) and [API guide](https://docs.z.ai/guides/overview/quick-start) distinguish subscription coding and general API endpoints. Custom endpoints, local models, and other OAuth connections are not wired for automatic runs yet. Successful installation/configuration probes do not establish paid-model access, subscription entitlement, or OS containment.

## Start a mission

Right-click a terminal pane and choose **New AI mission here…**, or click **New AI mission** in the top bar. The focused pane's Git top level (or its current folder) fills the repository path and **Inspect repository** runs immediately. When you type or change the path yourself, inspection also runs when you leave the field. The result shows the canonical path, current commit, and pending changes. Starting requires an existing commit. When changes are pending, you can start with **Start with the uncommitted changes included** ticked (the default): the daemon records that working tree as a private snapshot commit of its own and bases the mission on it. Your working directory, index, branch and HEAD are left exactly as they are — nothing is committed or stashed for you. Untracked files are included; `.gitignore`d ones are not. Clear the box to base the mission on the current commit (HEAD), which then requires a clean worktree. An in-progress merge or rebase, or an uncommitted set past the size bound, is refused — tidy up first. Up to 20 changed paths are listed, including untracked files.

Enter a specific goal. The first saved team template is selected automatically, and reloading the list never changes a team you picked. When no team exists or the selected team has unverified roles, a Quick setup card appears inside the dialog so you can create a team there.

Expand **Done when (optional)** to write requirements, choose saved verification commands for them, and mark requirements that need human confirmation. If you leave requirements empty, a single human-confirmed requirement "Confirm the goal is met: (first line of the goal)" is created, and you confirm the goal yourself at acceptance. If verification commands cannot run on this OS for the repository, the dialog blocks command selection and switches requirements to human checks.

**Review** is always visible. **Include review (recommended)** adds the Reviewer's independent review; **Skip review (faster)** accepts with verification commands and your own confirmation without a Reviewer role. A team without a Reviewer defaults to skipping review.

**Run settings** are collapsed by default and show a summary (parallelism, attempts, time limit in minutes, cost cap). Expand them to change parallelism, attempts per task, the run time limit in minutes (1–120), the cost cap, and **When cost cannot be established**. Cost input is in USD with up to six decimal places. Leaving the cap blank still applies the unknown-cost policy. Turn off **Apply plans automatically** if you want to apply or revise the Lead's plan yourself.

Click **Create and start**. If creation succeeds but starting fails, the primary button becomes **Start again**, which uses the same mission instead of creating another. If starting is rejected because the repository's base commit or registration changed, **Recreate from the current HEAD** (or **Recreate for the current repository**) appears: your inputs stay, the earlier draft is cancelled when possible, and the repository is inspected again before a new mission is created. If you go to settings while a draft has not started, the draft is kept open in a tab (or left in the mission list at the tab limit) and its location is announced. Continue with **Start** or **Delete draft** (cancel and archive, undoable right after) in the draft tab header.

Follow-up missions are opened from **Start a follow-up mission from this result** on a completed mission. If the previous mission was accepted and the repository is the same, the new mission sends the accepted result commit as its base (`expected_base_oid`) and records `follow_up_of`; the repository HEAD and uncommitted-change warnings are not shown. Otherwise it announces that it starts from the current HEAD. If the previous result commit changed, creation is rejected with `follow_up_base_mismatch`; reopen the dialog.

## Follow execution

The workflow is **goal → Lead plan → task runs → integrated result → verification → independent review → your acceptance**. Verification failures and major review findings trigger a repair plan within the configured budget.

The **task list** shows models, states, dependencies, and attempts. Each task's agent works in a separate daemon-owned Git worktree (workspace). After execution cleanup, the daemon captures actual changes and checks their allowed paths. A model's claim that tests passed is separate from the daemon's final verification evidence.

Answer requests in **Decision needed**. Applying a plan and creating its tasks are one transaction. An execution approval is delivered to its exact provider request.

### Retry a failed task

When a plan or task run fails, open **Decision needed** to inspect its error and attempt limit. Fix authentication, model settings, or input errors, then choose **Retry after fixing the cause**. To use another model, choose **Retry with a different model** in the failed task's details. Only enabled models allowed by the mission policy are available; the model selection and retry are saved together. A retry creates a new run for the same task and preserves previous failures and attempt counts. At the limit, adjust the limits or choose **Stop mission (record as failed)**. The final failure state waits for other owned executions to finish cleanup. Dependent tasks wait while unrelated tasks continue. A retry never resends messages whose delivery is unconfirmed.

When an agent reports that it cannot proceed (`provider_blocked`), the decision panel shows the agent report and reported code. Choose **Retry with instructions**, **Change model**, **Request a replacement plan**, or **Stop mission**.

### Stop a task's run

**Stop this task's run** stops only that execution (**Cancel task** when no run is active). The confirmation shows how many dependent tasks are affected. Required work is preserved: after termination is confirmed, use **Retry** or **Retry with a different model** in the same phase, or ask the Lead to replace the cancelled task and its dependents in a new plan. Cancelled verification and review tasks are not automatically recreated. Pending optional work must finish or be cancelled before advancing; retrying it after the phase advances requires a new plan. Replacements preserve the original requirements, execution budgets, and previous records. A cancelled required task blocks acceptance; the original run is never marked successful and requirements are not deleted.

Instructions queued behind a failed Lead wait for its recovery decision instead of repeatedly creating new plans. Retrying a paused mission leaves the task ready until **Resume**. Executions whose local termination is unconfirmed cannot be retried.

### Recover runs with unknown outcomes

For an `unknown` or `interrupted` execution, the daemon checks its persisted local termination evidence: the supervisor's `Exec.exited` record after owned group and output cleanup, the Run/Exec link, and the original launch manifest hash. Once verified, open **Decision needed → Start a new attempt after reviewing effects**. The provider outcome and completion of external actions remain unknown, so inspect previous changes and external effects first. To switch connections, use **Change model** in task details before choosing the new attempt.

The new attempt receives a new Run and workspace. The previous execution state, result, attempt history, and quarantined workspace remain available, and uncertain messages are not resent. Without termination evidence, model changes and retry remain blocked. A missing in-memory execution or PID lookup alone cannot confirm termination.

On Linux, executions with a kernel group identity saved before launch can reconnect to the same cgroup after restart. A live group is observed until a saved **Stop mission** or execution cancellation authorizes termination. Reservations are released only after the group is confirmed empty and its exit record is saved. The group evidence survives a failed save so recovery can retry after another restart.

New macOS executions use an independent observer that keeps tracking through a daemon restart. The new daemon verifies that observer and its original target before reconnecting or cancelling. Losing the observer itself does not confirm target termination. macOS recovery covers observed processes; descendants that escaped observation may remain. This limit appears in run details and the retry decision.

Older records without recovery identity, process trees without an independent observer, Windows native adoption, and provider result retrieval remain unsupported. Local termination does not settle unknown provider cost.

After restart, executions without confirmed termination still count against their original memory, CPU, and concurrency reservations. Archiving or ending a mission does not erase these reservations. If recovery records cannot be read or validated, new execution starts wait until a valid read succeeds. Completion and cancellation of processes already supervised by the current daemon continue during this hold.

### Confirm a stuck run yourself

A run that has no termination evidence and still holds an execution slot shows a one-line notice with **I confirmed the process has exited** in task details. Expanding it shows the recorded PID and process group and how to check (macOS/Linux: `ps -p <PID> -o pid,lstart,command` or Activity Monitor; Windows: the Details tab of Task Manager). PIDs can be reused, so compare the start time and command too. After ticking the checkbox, **Clear with my confirmation** records your attestation (`mission.run.attest_exited`) and releases the slot.

The daemon did not observe the exit. The run is labeled **Cleared by your confirmation · external effects unverified**; the provider outcome and external effects stay unverified, and the result screen still asks you to confirm this run's external effects before acceptance. If the daemon decides the run is not eligible, it rejects with `attestation_not_applicable`; refresh to see the current state. While a mission is stopping, **Open the first run** in the mission controls jumps to a run awaiting exit confirmation.

### Sending and resending messages

**Message the whole mission** goes to the Lead; **Send more instructions to this task** in task details goes to that task. Runs with steering support receive instructions during execution. Codex requires an acknowledgement for the exact turn before showing **Delivered**; this does not prove the instruction was carried out. Unsupported runs and paused missions keep instructions **Queued** for the next run or resume. If no active Lead or pending plan exists, the daemon creates a planning task. Resolve any blocking question, approval, or budget decision first. The automatic integration step has no instruction composer; send change requests to the Lead.

If a regular composer cannot confirm its save response, keep the text and recipient and select **Check send result again**. This resolves the same request without duplicating an already saved instruction, including after the task ends. Editing and a new request are enabled only after the server confirms rejection. Drafts and unresolved requests survive navigation between missions and detail panes. After restarting the app, reopen the same composer to restore a send that needs confirmation. For a task, open **Tasks → task details → Send more instructions to this task**; for a replacement, inspect its original message. The body is fetched using its saved reference and checked against its length and hash. Unsent drafts are retained only while the app remains open. A confirmed save and the provider's **Delivered** status are separate.

**Delivery unconfirmed** messages are never automatically resent or included as new instructions in another run. The earlier instruction may already have taken effect. Inspect its execution history, open **Review and resend**, edit the text, and choose **Send as a new message**. The new instruction keeps the same recipient and links to its original; both the earlier delivery state and execution history remain unchanged. **View original message / View replacement message** navigate between them. Each original can have one direct replacement. An unconfirmed replacement must itself be reviewed before another replacement is created. Decision and approval answers do not offer this action. If the send receipt is lost, the text stays fixed and **Check send result again** retries the exact same request.

If the recovery record cannot be read, new input stays locked and **Read recovery record again** is available. Instructions are not submitted until the recovery record is saved. A cleanup failure after saving the message also retains the same request for **Check send result again**. If another window has an unresolved send to this recipient, that request is restored first; this window's new draft reappears after confirming it. The original request's save result can still be checked if its text cannot be restored.

Delivered messages remain conversation history. Messages cannot change completion requirements or widen allowed file paths; use a follow-up mission to change requirements. Finished tasks reject new instructions.

### Pause, stop, and time

**Pause new runs** stops new launches and lets active runs finish. **Resume** continues scheduling. **Stop mission** completes after execution cleanup is confirmed. Closing a tab is separate from stopping its mission.

The header's **Active time** counts elapsed time while the mission is running or waiting to pause. Parallel run times are not added together; paused time and daemon downtime are excluded. Task details show **Recorded run time** for that individual execution. These are saved values, updated about once a second. A crash or storage failure can lose the unsaved interval, and unknown execution time is not estimated. Reaching the active-time limit blocks new runs and asks you to adjust limits in **Decision needed**.

**Limits for this mission…** in the mission controls edits the active time, automatic starts, repair cycles, attempts per task, parallel runs, cost cap, and unknown-cost policy next to current usage. Out-of-range values are adjusted to the allowed range. Limits can change only while the mission is running or paused, and new limits apply from the next scheduling check.

**Change the team…** in the same menu swaps the model behind each role. Lead and Builder — and Reviewer while independent review is on — cannot be left empty; optional roles such as Integrator can be set to **Nobody**. A connection that cannot run that role is shown with its reason and cannot be picked. The team can change while the mission is a draft, running, or paused; work already sent finishes on its original model and the next assignment uses the new team.

## Waiting for an automatic retry

A temporary startup or connection failure before the request reached the model can enter **Waiting to retry automatically** after local cleanup is confirmed. Open the task details for the scheduled time. The first retry waits about 2–2.4 seconds, and the second about 10–12 seconds; a later provider retry/reset time takes precedence. Attempt, total-start, time, model, and cost limits are checked before starting. At most two automatic retries are scheduled.

Each retry creates a new Run and workspace while preserving the previous failure, model snapshot, and workspace. Changing the model affects the next attempt and keeps the waiting time. Paused missions wait for resume; cancelling the task cancels its retry. Restarting the daemon or retrying a database save does not move the deadline.

This currently covers Codex transport failures before its request, Claude initialization transport failures before the user frame, and OpenCode server readiness failures before submission. Authentication rejection, model/policy errors, and uncertain delivery remain explicit recovery cases. Plan format errors follow the separate correction rules below. Other failures after submission require assessment of the previous run's external effects before any automatic retry.

## Waiting for a plan format correction

Invalid JSON or plan structure in a completed Lead answer can enter **Waiting for plan format correction**. After execution cleanup is confirmed, Lead receives the validator error and rejected answer in a new run. At most two automatic corrections are allowed within task-attempt, total-start, time, model and cost limits. The next run is asked for a complete corrected plan using the diagnostic.

No part of a rejected plan is applied. The original requirements, task contract, failed evidence and previous workspace remain intact; each new run uses a separate workspace. Paused missions wait for resume. **Change model** affects the next run, and cancelling the task cancels correction. If an intervening attempt provably never submitted the request, the next attempt keeps the same correction context.

Disallowed paths, roles, models or verification commands, authentication failures, stale plan revisions and unknown outcomes do not trigger format correction. Missing, unavailable, unowned or oversized context also prevents automatic execution. If corrections fail twice or current limits/configuration prevent another attempt, the notice becomes **Plan auto-correction stopped · decision needed**; open **Decision needed** to review the cause and choose the next action.

## Follow a required-task replacement plan

When a required task reaches its attempt limit, Lead receives its failure evidence and original requirements to propose a **Required-task repair plan**. Open **Tasks → task details** to see the failed task and the Lead handling the repair. The plan must include required replacements; it waits for approval if automatic plan adoption is disabled. Unrelated tasks continue.

Previous runs, workspaces, and attempt counts stay intact. Replacements use new runs and workspaces. The original task cannot start a separate retry while Lead owns its recovery. **Change model** on a pending Lead changes its next assignment without consuming another attempt. Cancelling the repair plan prevents automatic recreation for the same failed run.

The repair-cycle limit in **Limits for this mission…** is shared by required-task, verification, and review repairs. If required work or mandatory verification exhausts that limit, the mission cleans up remaining executions before becoming failed. A limit of zero disables automatic repair. Missing termination evidence keeps the mission waiting for confirmed cleanup.

Major or blocking review findings retain a user decision path. Inspect them in **Result** and dismiss them with a recorded reason when justified, increase the repair limit, or stop the mission. Resolving all major and blocking findings on the current result clears its review-limit decision and resumes acceptance checks. Finding dismissal cannot override failed mandatory verification.

## When a task waits for cost admission

Each new provider run reserves the estimate saved in its model connection. With a $1 cap and two tasks estimated at $0.60 each, only one starts. If that run finishes with a reported cost of $0.10, its unused reservation is released and the next task can start. A run without a final reported cost retains its estimate. Reported costs from failed and cancelled runs also count.

Blocking unknown cost holds a task when its model has no estimate or a previous run still has unpriced cost. Support for reporting usage after execution does not establish an estimate before execution. Allowing unknown cost retains the cap checks for known costs and reservations. Unknown cost is shown explicitly, and subscription quotas are never converted to dollars.

Open the cost question in **Decision needed**, choose **Adjust this mission's limits**, and save a new cap or unknown-cost policy. The next scheduling pass releases eligible tasks. To use another allowed model, choose **Change model** in the waiting task's details. You can also change the estimate in model settings. Each already reserved run keeps its original model and estimate, so editing a connection cannot fill in unknown historical cost. Unrelated tasks can continue if their cost checks pass. You can also stop the mission.

Usage in **Result** separates provider-reported amounts, reservations and estimates, and unpriced run counts. The cap controls new starts; a provider already running may exceed its estimate.

## When the provider limits requests

A structured provider rejection with a valid reset time holds new tasks using that model connection (**Waiting for the provider rate limit**). Open the waiting task's details to see the reset in local time. After the deadline, scheduling checks the model, dependencies, cost, time, and other limits again. Waiting does not create runs or consume attempts. Observed limits survive daemon restarts and mission archiving.

Tasks on other connections can proceed. **Change model** selects another allowed connection and checks its limits. Editing only the label or estimated cost keeps the original hold. Paused missions do not launch after a reset; running missions continue to accumulate active time while waiting, so pause for long waits when needed.

The hold controls new reservations. It does not fail a running provider or replay a failed run. Failed runs need a recovery decision unless they satisfy the verified pre-submission retry conditions above. Supported signals are Claude's explicit `rejected`/`resetsAt`, Codex's explicit reached state and corresponding reset window, and OpenCode HTTP 429 with `Retry-After`. Percentages alone and missing, expired, or invalid timestamps cannot establish a reset. Sparse normal updates do not shorten an already recorded deadline. Rechecking externally changed accounts and mapping one account across several connections remain separate work.

## Automatic integration

Once the task results are captured, an automatic integration step appears in the task list. It owns a Run, workspace, and durable Exec with an admission estimate of 512 MiB and one CPU slot. It waits for capacity without creating another attempt, and counts toward automatic-start and per-run time budgets.

Its input consists of recorded results. Send change requests to the Lead. An automatic Run displays no model and has no instruction composer. **Conflict integrator** in the task detail identifies the model assigned to a future resolution Run. After cancelling the initial automatic step and confirming cleanup, **Retry** creates a new Run and workspace while preserving the old attempt. Stopping the entire mission does not start another attempt.

Cancellation and timeout wait for owned Git descendants to end. Temporary exit-record or result-publication failures retry persistence without rerunning Git. A daemon crash leaves the result unknown and follows the recovery procedure above.

For a conflict, open the decision banner to inspect the affected files. You can change the assigned model in the integration task detail before selecting **Ask the integrator to resolve**. An open conflict decision cannot be bypassed with generic Retry. While paused, the decision is saved and execution waits for resume.

A new Run on the same Task exclusively reuses the retained integration workspace. The failed Run remains unchanged. After the model reports completion, another automatic Run captures the actual files and applies the remaining original results in order. A new result receives fresh verification and review before acceptance. Another conflict in a later result opens another decision on the same Task/workspace.

Remaining conflict markers or changes outside the original writer scopes fail without publishing a result. Explicit **Retry** returns to the Integrator in a new Run to inspect and fix the files. Both automatic and model Runs consume attempt, time, and automatic-start budgets; model Runs also use the assigned connection's concurrency and cost policy. Exhausted limits require a limit adjustment.

The current resolution path handles regular files and explicit deletions without following symlinks. If the daemon restarts during resolution or continuation, previous files remain quarantined. After local exit is confirmed, choose **Rebuild integration and preserve quarantined output** to reconstruct the original results in a fresh workspace. Previous files are not automatically adopted; another conflict needs a new decision. Special files, LFS, and submodules remain additional work.

To omit the conflicting result, choose **Exclude the conflicting change and ask the Lead to replan**. The next integration excludes that result and work depending on it, including work based on a result containing its output. Previous successful tasks, runs, results, and files remain unchanged. The Lead must replace excluded required tasks while preserving their original requirements; excluded optional tasks can be omitted. Required work cannot be waived through exclusion.

Excluding an input from an earlier integrated result also requires rebuilding repair work and checks based on that result. The Lead must preserve required verification/review tasks and their verification commands. These checks run after the new integration and must succeed on the new result. Repeated exclusions preserve the replacement chain and original completion requirements.

New work starts without the excluded result as its base. The new integration receives fresh verification and review, and its manifest records the exclusion decision. This action consumes a repair cycle. While paused, the decision and plan are saved, with execution waiting for resume. Changed conflict inputs or missing termination evidence prevent exclusion.

Past uncertain Runs still block acceptance by default. In the result screen's **Check yourself** list, check only the Runs whose previous changes and external effects you have reviewed. The server rechecks termination evidence, the explicit recovery decision, and the completed replacement, then retains your review in the acceptance event. Runs without termination evidence cannot be checked. A different result or proof requires a new confirmation. The old outcome stays unknown.

## Verification execution and recovery

Verification commands run in a separate worktree at the current result. Configure an absolute executable path or a bare name available on the daemon's PATH, with a separate argument list. Relative executable paths and environment profiles are currently unsupported. The working directory must resolve inside the verification worktree.

Each verification owns a durable execution record and reserves an estimated 512 MiB and one CPU slot while capacity is available; these are admission estimates, not hard usage caps. The command and mission run limits both apply. Cancellation and timeouts wait for owned-process cleanup before recording the result. Transient database failures retry the same log upload or result transaction without running the command again. A daemon crash can leave the result uncertain. Supported Linux cgroups and the macOS observer can recover local ownership and stop the remaining process, while the original verification remains unconfirmed until an explicit recovery decision.

New macOS verifications run with a separate environment and an OS sandbox. Writes are restricted to a separate output directory; the result files and other host paths remain read-only to the command and its children. Network access requires permission in both the command and mission policy. API keys, SSH-agent settings and runtime injection variables are not inherited. New verification on other operating systems currently fails with `verification_unsupported_os` because a verified isolation backend is not available. When repository inspection reports `verification_supported: false`, the new mission dialog blocks command selection and defaults requirements to human checks. Historical execution recovery and observed results remain readable.

Write generated files to the directory in `IYAGI_VERIFICATION_OUTPUT`. HOME, temporary and XDG directories use that directory; Cargo output goes into its target subdirectory. Commands that write caches or results into source directories need a separate output setting. Output is retained beside the verification worktree. Custom environment profiles and enforced disk quotas remain unsupported.

Verification inputs are created directly from the result's raw file data, without Git hooks, external conversion filters or line-ending conversion. Raw input is limited to 64 MiB per file and 512 MiB in total. File bytes, executable modes and links are compared with the result; external links and unsupported entry types are rejected. A successful sandboxed command whose input matches before and after execution receives **Input write-blocking enforced**, including for strict acceptance. Failed starts or changed input cannot establish that guarantee. Host file reads are not isolated. The documented macOS process-observation limits still apply.

## Accept the result

Integration uses each result's recorded commit. Moving its Git ref later does not substitute new contents. Missing original objects or inconsistent source-run provenance block integration. The result manifest and conflict question record the result identities and exact source base, commit, and tree OIDs.

In **Result**, inspect the current result, verification logs, and review. A new result needs fresh verification and review. If the result changes after you open the screen, acceptance is disabled until you refresh, and all your checks are cleared. Explicitly acknowledge verification whose input write protection was observed rather than enforced, and confirm human-check requirements.

The acceptance checklist mirrors the daemon's `acceptance_ready`. The daemon returns only the first rejection reason, while the screen lists every unmet item in the daemon's order and shows the first one next to the accept button. The button enables only when the phase is awaiting acceptance and every condition is met.

Acceptance records that result as the mission's completed result. It does not merge your original branch, push, or deploy. The result commit is pinned on the private ref `refs/iyagi/missions/<mission id>/candidates/<result id>`; workspace worktrees share objects and refs with your repository, so the commit is usable there directly. The result commit descends from the base commit, so `git merge --ff-only` works while your branch is still at the base. A result without changes has the same commit as its base.

## How workspace cleanup works

`workspace.usage` reads each mission's workspace count, size, and whether it can be cleaned; `workspace.cleanup` runs only when you click. Nothing is deleted automatically. If usage cannot be read, the screens hide it quietly. Cleanup is rejected for an active mission (`mission_active`) or a mission with a run whose exit is unconfirmed (`run_unreconciled`). The response lists the number removed, the space freed, and kept items with reasons (`dirty`, `run_active`, `not_daemon_owned`, `quarantined`, `unregistered_worktree`, `repository_unavailable`, `status_unavailable`, `deferred`, `remove_failed`). `quarantined` workspaces belong to a run whose outcome was uncertain or confirmed by you, and are kept for inspection. `deferred` items were skipped for time; cleaning up again continues with them.

## Implementation and evidence

The daemon starts the actor in `crates/iyagi-termd/src/lib.rs`. `mission/actor.rs` drives provider adapters and deterministic integration and verification workers; `engine.rs` reserves runs against the global scheduler; `planning.rs` validates and adopts plans; `execution.rs` prepares owned workspaces and persists fenced results; `pipeline.rs` and `workflow.rs` advance results through checks, review, repairs, and acceptance. `exec/` owns pipe-process admission, launch gates, native groups, and confirmed cleanup. `mission/exec_store.rs` persists owned launch manifests and links each Exec to its Run; the daemon Codex, Claude, and OpenCode factories and `mission/verification_exec.rs` / `mission/integration_exec.rs` / `mission/integration_recovery.rs` use this shared supervisor. The verifier freezes its exact command, result, workspace and admission policy before the process can start. Integration freezes source results and their commit/tree identities, then owns all Git commands through a dedicated helper Exec. `connections.rs` resolves saved credentials and endpoint metadata for all three runtimes. Codex's `auth.rs` and Claude's `auth.rs`/`authenticated.rs` configure authentication and check it before sending a request. Runtime-specific protocols live under `crates/iyagi-termd/src/agent_runtime/{codex,claude,opencode}`. SQLite projection, event, request, and outbox updates share a transaction in `crates/term-storage/src/mission/ops.rs`.

`exec/native_recovery.rs` verifies persisted ownership before observing or cancelling a previous execution. The platform implementations are `crates/term-platform/src/group/linux_recovery.rs` for pinned Linux cgroups and `macos_guardian.rs` for the independent macOS observer. `mission/attestation.rs` records a user's exit confirmation for a stuck run, and `mission/workspace_cleanup.rs` reports and cleans mission workspaces.

`mission/failure_repair.rs` assigns Lead to confirmed exhausted failures, validates required replacements, and waits for execution cleanup before final failure.

`mission/plan_repair.rs` schedules bounded plan format corrections and supplies the previous answer and validator diagnostic. `mission/transient_retry.rs` handles separate delayed retries for proven unsubmitted requests.

`mission_actor` tests inject adapter events while using real Git, SQLite, and verification commands. The `actor_goal_to_acceptance_over_real_daemon_git_and_protocol_processes` test uses a real daemon and live protocol child processes, with deterministic fixture output in place of model inference. The installed Codex combination has separate [real-model full mission evidence](CODEX_MACOS_MISSION_01540.md). Other provider, OS, and model combinations still require separate evidence.
