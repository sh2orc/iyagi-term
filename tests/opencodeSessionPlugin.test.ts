import { EventEmitter } from "node:events";
import { pathToFileURL } from "node:url";
import { resolve } from "node:path";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const { spawn } = vi.hoisted(() => ({ spawn: vi.fn() }));
vi.mock("node:child_process", () => ({ spawn }));
const moduleUrl = (name: string) => pathToFileURL(resolve(`crates/iyagi-termd/src/opencode_integration/${name}.mjs`)).href;
const { default: serverPlugin } = await import(moduleUrl("session"));
const { default: tuiPlugin } = await import(moduleUrl("tui"));
const { default: reporter } = await import(moduleUrl("reporter"));
const reports: Record<string, string>[] = [];

beforeEach(() => {
  reports.length = 0;
  vi.stubEnv("IYAGI_DAEMON_BIN", "/app path/iyagi-termd");
  vi.stubEnv("IYAGI_DATA_DIR", "/app data");
  vi.stubEnv("IYAGI_SESSION_ID", "terminal-1");
  vi.stubEnv("IYAGI_WORKLOAD_ID", "workload-1");
  spawn.mockReset().mockImplementation(() => {
    const child = Object.assign(new EventEmitter(), {
      stdin: Object.assign(new EventEmitter(), {
        end(payload: string) { reports.push(JSON.parse(payload)); Promise.resolve().then(() => child.emit("close", 0)); },
      }),
      kill: vi.fn(),
    });
    return child;
  });
});
afterEach(() => { vi.unstubAllEnvs(); vi.useRealTimers(); });

describe("OpenCode terminal session reporters", () => {
  it("records roots and resumed prompts, ignores child sessions and background completion", async () => {
    const get = vi.fn(async ({ path }: { path: { id: string } }) => ({ data: {
      id: path.id, directory: "/repo", parentID: path.id === "ses_child" ? "ses_first" : undefined,
    } }));
    const hooks = await serverPlugin({ client: { session: { get } }, directory: "/repo" });
    const created = (id: string, parentID?: string) => hooks.event({ event: {
      type: "session.created", properties: { info: { id, parentID, directory: "/repo" } },
    } });
    await created("ses_first");
    await created("ses_child", "ses_first");
    await hooks["chat.message"]({ sessionID: "ses_child", prompt: "must not be persisted" });
    await hooks["chat.message"]({ sessionID: "ses_resumed", prompt: "must not be persisted" });
    await hooks.event({ event: { type: "session.idle", properties: { sessionID: "ses_first" } } });
    expect(reports).toEqual([
      { hook_event_name: "SessionStart", session_id: "ses_first", cwd: "/repo" },
      { hook_event_name: "SessionStart", session_id: "ses_resumed", cwd: "/repo" },
    ]);
    expect(spawn).toHaveBeenCalledWith("/app path/iyagi-termd",
      ["--data-dir", "/app data", "hook", "--agent", "opencode"], expect.objectContaining({ stdio: ["pipe", "ignore", "ignore"] }));
  });

  it("tracks existing conversations selected in the TUI without sending a prompt", async () => {
    vi.useFakeTimers();
    const route = { current: { name: "session", params: { sessionID: "ses_first" } } };
    let dispose: (() => void) | undefined;
    await tuiPlugin.tui({
      route, lifecycle: { onDispose: (fn: () => void) => { dispose = fn; } },
      state: { path: { directory: "/repo" }, session: {
        get: (id: string) => ({ id, directory: "/repo", parentID: id === "ses_child" ? "ses_first" : undefined }),
      } },
    });
    await vi.advanceTimersByTimeAsync(250);
    route.current.params.sessionID = "ses_child";
    await vi.advanceTimersByTimeAsync(250);
    route.current.params.sessionID = "ses_selected";
    await vi.advanceTimersByTimeAsync(500);
    dispose?.();
    route.current.params.sessionID = "ses_after_dispose";
    await vi.advanceTimersByTimeAsync(500);
    expect(reports.map(r => r.session_id)).toEqual(["ses_first", "ses_selected"]);
  });

  it("does not report outside a iyagi terminal or accept an ID that could become a CLI flag", async () => {
    await reporter()("SessionStart", "--auto", "/repo");
    vi.stubEnv("IYAGI_SESSION_ID", "");
    await reporter()("SessionStart", "ses_good", "/repo");
    expect(spawn).not.toHaveBeenCalled();
  });

  it("a missing daemon never rejects the OpenCode hook", async () => {
    spawn.mockImplementation(() => { throw new Error("ENOENT"); });
    await expect(reporter()("SessionStart", "ses_good", "/repo")).resolves.toBeUndefined();
  });
});
