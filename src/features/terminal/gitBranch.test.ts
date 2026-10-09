import { describe, expect, it, vi } from "vitest";
import type { IpcAdapter } from "../bridge/ipc";
import { queryGitBranch } from "./gitBranch";

function ipc(result: unknown): IpcAdapter {
  return {
    invoke: vi.fn(async () => result) as IpcAdapter["invoke"],
    channel: () => ({}),
  };
}

describe("terminal Git branch lookup", () => {
  it("passes the terminal cwd to the hardened native command", async () => {
    const adapter = ipc({ name: "feature/terminal-header", detached: false, top_level: "/repo/project" });
    await expect(queryGitBranch("/repo/project", adapter, true)).resolves.toEqual({
      name: "feature/terminal-header",
      detached: false,
      top_level: "/repo/project",
    });
    expect(adapter.invoke).toHaveBeenCalledWith("system_git_branch", { cwd: "/repo/project" });
  });

  it("stays hidden outside Tauri and degrades to null on lookup errors", async () => {
    const adapter = ipc(null);
    await expect(queryGitBranch("/repo", adapter, false)).resolves.toBeNull();
    expect(adapter.invoke).not.toHaveBeenCalled();
    const failing = { ...adapter, invoke: vi.fn(async () => { throw new Error("gone"); }) as IpcAdapter["invoke"] };
    await expect(queryGitBranch("/repo", failing, true)).resolves.toBeNull();
  });
});
