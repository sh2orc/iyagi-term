import { describe, expect, it } from "vitest";

import type { IpcAdapter } from "./ipc";
import { mockSystemProbe, tauriSystemProbe } from "./systemProbe";

function fakeIpc(handlers: Record<string, (args: Record<string, unknown>) => unknown>) {
  const calls: Array<{ command: string; args?: Record<string, unknown> }> = [];
  const ipc: IpcAdapter = {
    invoke<T>(command: string, args?: Record<string, unknown>): Promise<T> {
      calls.push({ command, args });
      const handler = handlers[command];
      if (!handler) return Promise.reject(new Error(`no handler for ${command}`));
      return Promise.resolve(handler(args ?? {}) as T);
    },
    channel<T>(_onMessage: (message: T) => void): unknown {
      return {};
    },
  } as IpcAdapter;
  return { ipc, calls };
}

describe("systemProbe.locateProgram", () => {
  it("invokes system_locate_program with the bare name and returns the path", async () => {
    const { ipc, calls } = fakeIpc({
      system_locate_program: (args) =>
        args.name === "node" ? "C:\\Program Files\\nodejs\\node.exe" : null,
    });
    const probe = tauriSystemProbe(ipc);
    await expect(probe.locateProgram?.("node")).resolves.toBe("C:\\Program Files\\nodejs\\node.exe");
    await expect(probe.locateProgram?.("python3")).resolves.toBeNull();
    expect(calls.map((c) => c.command)).toEqual(["system_locate_program", "system_locate_program"]);
    expect(calls[0]?.args).toEqual({ name: "node" });
  });

  it("mock probe answers node only", async () => {
    const probe = mockSystemProbe();
    await expect(probe.locateProgram?.("node")).resolves.toMatch(/node\.exe$/);
    await expect(probe.locateProgram?.("deno")).resolves.toBeNull();
  });
});
