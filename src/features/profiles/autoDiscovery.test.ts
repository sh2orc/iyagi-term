import { describe, expect, it } from "vitest";
import type { SystemProbe } from "./probeTypes";
import { autoConfigureDetectedProfiles } from "./autoDiscovery";
import { createProfilesStore, type ProfilesStorage } from "./profileStore";

class MemoryStorage implements ProfilesStorage {
  private values = new Map<string, string>();
  getItem(key: string): string | null { return this.values.get(key) ?? null; }
  setItem(key: string, value: string): void { this.values.set(key, value); }
  removeItem(key: string): void { this.values.delete(key); }
}

const probe: SystemProbe = {
  async listClis() {
    return [
      { program: "/nvm/bin/codex", kind: "codex", resolvedTarget: null, installForm: "symlink" },
      { program: "/local/bin/claude", kind: "claude", resolvedTarget: null, installForm: "native" },
    ];
  },
  async queryVersion(program) {
    return program.includes("codex") ? "0.153.4" : "2.1.266";
  },
};

function testStore() {
  const storage = new MemoryStorage();
  let id = 0;
  return createProfilesStore(() => storage, () => `id-${++id}`);
}

describe("built-in profile auto discovery", () => {
  it("fills untouched built-ins and leaves missing tools empty", async () => {
    const store = testStore();
    expect(await autoConfigureDetectedProfiles(probe, store)).toBe(2);
    expect(store.getState().profileByLabel("codex")?.descriptor).toMatchObject({
      program: "/nvm/bin/codex",
      detected_version: "0.153.4",
    });
    expect(store.getState().profileByLabel("claude")?.descriptor.program).toBe("/local/bin/claude");
    expect(store.getState().profileByLabel("opencode")?.descriptor.program).toBe("");
  });

  it("never overwrites a user-selected program", async () => {
    const store = testStore();
    const codex = store.getState().profileByLabel("codex")!;
    store.getState().updateProfile(codex.id, {
      descriptor: { ...codex.descriptor, program: "/my/codex" },
    });
    await autoConfigureDetectedProfiles(probe, store);
    expect(store.getState().profileByLabel("codex")?.descriptor.program).toBe("/my/codex");
  });
});

describe("Windows npm shim 처리(W3 #4)", () => {
  const CLAUDE_CLI = "C:\\Users\\u\\AppData\\Roaming\\npm\\node_modules\\@anthropic-ai\\claude-code\\cli.js";
  const claudeShim = {
    program: "C:\\Users\\u\\AppData\\Roaming\\npm\\claude.cmd",
    kind: "claude" as const,
    resolvedTarget: CLAUDE_CLI,
    installForm: "cmd-shim",
  };
  const codexShim = {
    program: "C:\\Users\\u\\AppData\\Roaming\\npm\\codex",
    kind: "codex" as const,
    resolvedTarget: null,
    installForm: "script",
  };
  const codexExe = {
    program: "C:\\Users\\u\\.local\\bin\\codex.exe",
    kind: "codex" as const,
    resolvedTarget: null,
    installForm: "native",
  };

  it("같은 종류에 native 후보가 있으면 shim이 먼저 나열돼도 native를 고른다", async () => {
    const store = testStore();
    const winProbe: SystemProbe = {
      async listClis() {
        return [codexShim, codexExe];
      },
      async queryVersion() {
        return "0.20.0";
      },
    };
    expect(await autoConfigureDetectedProfiles(winProbe, store)).toBe(1);
    const codex = store.getState().profileByLabel("codex")!;
    expect(codex.descriptor.program).toBe(codexExe.program);
    expect(codex.interpreter).toBeNull();
  });

  it("shim뿐이고 node 경로를 모르면 건너뛴다 — 띄울 수 없는 프로필을 만들지 않는다", async () => {
    const store = testStore();
    const winProbe: SystemProbe = {
      async listClis() {
        return [claudeShim];
      },
      async queryVersion() {
        return "2.0.0";
      },
    };
    expect(await autoConfigureDetectedProfiles(winProbe, store)).toBe(0);
    expect(store.getState().profileByLabel("claude")?.descriptor.program).toBe("");
  });

  it("shim뿐이어도 브리지가 node를 찾아 주면 interpreter 형태(node + cli.js)로 자동 등록한다", async () => {
    const store = testStore();
    const NODE = "C:\\Program Files\\nodejs\\node.exe";
    const winProbe = {
      async listClis() {
        return [claudeShim];
      },
      async queryVersion() {
        return "2.0.0";
      },
      async locateProgram(name: string) {
        return name === "node" ? NODE : null;
      },
    } satisfies SystemProbe & { locateProgram(name: string): Promise<string | null> };
    expect(await autoConfigureDetectedProfiles(winProbe, store)).toBe(1);
    const claude = store.getState().profileByLabel("claude")!;
    // program은 shim 그대로(셸 모드·신원 표시), 관리 실행은 interpreter로.
    expect(claude.descriptor.program).toBe(claudeShim.program);
    expect(claude.descriptor.detected_version).toBe("2.0.0");
    expect(claude.interpreter).toEqual({ executable: NODE, scriptArgvPrefix: [CLAUDE_CLI] });
  });
});
