/**
 * System probe seam (ticket I04 bridge, consumed by profiles/I11).
 *
 * `SystemProbe` is the exact contract `src/features/profiles/probeTypes.ts`
 * mirrors — names are the seam, keep them stable. The tauri implementation
 * invokes the Rust bridge commands (`system_list_clis`,
 * `system_query_version`); the Rust side scans PATH/install dirs and runs
 * only the fixed, verified version query per CLI (2s timeout / 8 KiB cap,
 * 04 §5). The mock returns deterministic fixtures for previews and tests.
 */

import type { IpcAdapter } from "./ipc";
import type { DetectedShell } from "../terminal/shellProfiles";

export type CliCandidateKind = "codex" | "claude" | "opencode" | "custom";

export interface CliCandidate {
  /** Discovered executable/shim path, as-is. */
  program: string;
  kind: CliCandidateKind;
  /** Symlink/shim-resolved target; null when it cannot be resolved. */
  resolvedTarget: string | null;
  /** Install form label, e.g. "native" | "symlink" | "cmd-shim". */
  installForm: string | null;
}

export interface SystemProbe {
  listClis(): Promise<CliCandidate[]>;
  queryVersion(program: string): Promise<string | null>;
  /** 감지된 셸(PowerShell/pwsh/CMD/WSL 배포판) — 셸 프로필 UI용. */
  listShells(): Promise<DetectedShell[]>;
  /**
   * PATH에서 맨 이름(`node`)의 실행 파일을 찾는다 — npm shim의 `cli.js`를
   * 인터프리터 형식으로 자동 구성할 때 쓴다. 경로는 거부되고 못 찾으면 null.
   */
  locateProgram?(name: string): Promise<string | null>;
}

/**
 * Verified version query per CLI (04 §5). Unknown programs are NEVER
 * executed — `queryVersion` returns null instead of guessing a flag.
 */
const VERSION_ARGS: Readonly<Record<Exclude<CliCandidateKind, "custom">, string>> = {
  codex: "--version",
  claude: "--version",
  opencode: "--version",
};

function kindForProgram(program: string): CliCandidateKind {
  const base = program.replace(/^.*[/\\]/, "").toLowerCase();
  if (base.startsWith("codex")) return "codex";
  if (base.startsWith("claude")) return "claude";
  if (base.startsWith("opencode")) return "opencode";
  return "custom";
}

/** Probe backed by the Rust bridge commands. */
export function tauriSystemProbe(ipc: IpcAdapter): SystemProbe {
  return {
    async listClis(): Promise<CliCandidate[]> {
      return ipc.invoke<CliCandidate[]>("system_list_clis");
    },
    async queryVersion(program: string): Promise<string | null> {
      const kind = kindForProgram(program);
      if (kind === "custom") return null; // 검증된 query 없음 → null
      const arg = VERSION_ARGS[kind];
      return ipc.invoke<string | null>("system_query_version", { program, arg });
    },
    async listShells(): Promise<DetectedShell[]> {
      return ipc.invoke<DetectedShell[]>("system_list_shells");
    },
    async locateProgram(name: string): Promise<string | null> {
      return ipc.invoke<string | null>("system_locate_program", { name });
    },
  };
}

/** Deterministic fixtures — same array identity-free content every call. */
const MOCK_CLIS: readonly CliCandidate[] = [
  {
    program: "C:/Users/dev/.local/bin/codex.exe",
    kind: "codex",
    resolvedTarget: "C:/Users/dev/.local/bin/codex.exe",
    installForm: "native",
  },
  {
    program: "C:/Program Files/nodejs/claude.cmd",
    kind: "claude",
    resolvedTarget: null,
    installForm: "cmd-shim",
  },
  {
    program: "/usr/local/bin/opencode",
    kind: "opencode",
    resolvedTarget: "/opt/homebrew/bin/opencode",
    installForm: "symlink",
  },
];

const MOCK_VERSIONS: Readonly<Record<string, string>> = {
  codex: "0.20.0",
  claude: "1.0.32",
  opencode: "0.9.6",
};

/** Mock probe for dev preview in a plain browser and unit tests. */
const MOCK_SHELLS: readonly DetectedShell[] = [
  {
    program: "C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe",
    kind: "powershell",
    distro: null,
  },
  { program: "C:\\Program Files\\PowerShell\\7\\pwsh.exe", kind: "pwsh", distro: null },
  { program: "C:\\Windows\\System32\\wsl.exe", kind: "wsl", distro: "Ubuntu" },
  { program: "C:\\Windows\\System32\\cmd.exe", kind: "cmd", distro: null },
];

export function mockSystemProbe(): SystemProbe {
  return {
    async listClis(): Promise<CliCandidate[]> {
      return MOCK_CLIS.map((candidate) => ({ ...candidate }));
    },
    async queryVersion(program: string): Promise<string | null> {
      const kind = kindForProgram(program);
      if (kind === "custom") return null;
      return MOCK_VERSIONS[kind] ?? null;
    },
    async listShells(): Promise<DetectedShell[]> {
      return MOCK_SHELLS.map((shell) => ({ ...shell }));
    },
    async locateProgram(name: string): Promise<string | null> {
      return name === "node" ? "C:\\Program Files\\nodejs\\node.exe" : null;
    },
  };
}
