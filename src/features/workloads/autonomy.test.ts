import { describe, expect, it } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import { createElement } from "react";
import { autonomyArgv, effectiveLaunchCommand } from "./autonomy";
import { AutonomyToggle } from "./AutonomyToggle";
import { defaultCliCapabilities } from "../profiles/types";

const claudeFlag = "--dangerously-skip-permissions";
const codexFlag = "--dangerously-bypass-approvals-and-sandbox";

describe("CLI autonomy launch arguments", () => {
  it("defaults to full autonomy for Claude and Codex only", () => {
    expect(autonomyArgv("claude", [])).toEqual([claudeFlag]);
    expect(autonomyArgv("codex", [])).toEqual([codexFlag]);
    expect(autonomyArgv("custom", ["--full-auto"])).toEqual(["--full-auto"]);
    expect(autonomyArgv("opencode", [])).toEqual([]);
  });

  it("removes Claude permission flags, modes and duplicates when unchecked", () => {
    const args = [claudeFlag, "--allow-dangerously-skip-permissions", "--permission-mode", "bypassPermissions", "--permission-mode=auto", "--model", "sonnet", "--resume"];
    expect(autonomyArgv("claude", args, false)).toEqual(["--model", "sonnet", "--resume"]);
    expect(autonomyArgv("claude", args, true)).toEqual([claudeFlag, "--model", "sonnet", "--resume"]);
  });

  it("removes Codex aliases, legacy options, separate and attached permission values", () => {
    const args = [codexFlag, "--yolo", "--full-auto", "--approve-for-me", "--dangerously-bypass-hook-trust",
      "--ask-for-approval", "never", "-a=never", "-anever", "--sandbox=danger-full-access", "-s", "workspace-write", "-sdanger-full-access", "resume", "--last"];
    expect(autonomyArgv("codex", args, false)).toEqual(["resume", "--last"]);
    expect(autonomyArgv("codex", args)).toEqual([codexFlag, "resume", "--last"]);
  });

  it("removes permission config overrides without discarding model config", () => {
    const args = ["-c", 'approval_policy="never"', "--config=sandbox_mode=\"danger-full-access\"",
      "-csandbox_permissions=['disk-full-read-access']", "--config", 'permissions.network="allow"',
      "-c", 'model="gpt-test"', "--config=model_reasoning_effort=high", "--search"];
    expect(autonomyArgv("codex", args, false)).toEqual(["-c", 'model="gpt-test"', "--config=model_reasoning_effort=high", "--search"]);
  });

  it("preserves prompt data and the end-of-options boundary", () => {
    const args = ["--system-prompt", claudeFlag, "--", claudeFlag];
    expect(autonomyArgv("claude", args, false)).toEqual(args);
    expect(autonomyArgv("claude", ["--", "a prompt"])).toEqual([claudeFlag, "--", "a prompt"]);
    expect(autonomyArgv("codex", ["--ask-for-approval", "--model", "test"], false)).toEqual(["--model", "test"]);
  });

  it("normalizes profile and extra arguments together after the interpreter prefix", () => {
    const profile = {
      descriptor: { kind: "claude" as const, program: "C:\\cli.cmd", argv_prefix: ["--permission-mode", "auto"],
        detected_version: null, transport: "pty" as const, capabilities: defaultCliCapabilities() },
      interpreter: { executable: "C:\\node.exe", scriptArgvPrefix: ["C:\\cli.js"] },
    };
    expect(effectiveLaunchCommand(profile, [claudeFlag, "--model", "sonnet"], false)).toEqual({
      program: "C:\\node.exe", argv: ["C:\\cli.js", "--model", "sonnet"],
    });
    expect(effectiveLaunchCommand(profile, [claudeFlag]).argv).toEqual(["C:\\cli.js", claudeFlag]);
    expect(profile.descriptor.argv_prefix).toEqual(["--permission-mode", "auto"]);
  });

  it("shows an accessible, checked control for each supported CLI", () => {
    for (const kind of ["claude", "codex"] as const) {
      const html = renderToStaticMarkup(createElement(AutonomyToggle, { kind, description: true }));
      expect(html).toContain('type="checkbox"');
      expect(html).toContain('checked=""');
      expect(html).toContain('aria-describedby=');
    }
    expect(renderToStaticMarkup(createElement(AutonomyToggle, { kind: "opencode" }))).toBe("");
  });
});
