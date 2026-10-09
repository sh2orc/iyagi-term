/**
 * 셸 프로필 (04-ui §1 "셸 프로필" MVP, iTerm2 로드맵 표).
 *
 * Windows에서는 PowerShell/pwsh/CMD/WSL 배포판을 선택해 실행한다. 기본값은
 * 설정(store)에 두고 `+ Terminal` 버튼의 빠른 선택으로 pane마다 다른 셸을
 * 띄울 수 있다. WSL 진입은 `wsl.exe -d <배포판> --cd ~`(native 실행 파일 +
 * 인자 전달 — 02-runner §3의 managed shim 규칙과 무관하게 shell 모드는
 * 사용자가 지정한 프로그램을 그대로 실행).
 */

import type { Platform } from "./shortcuts";

export type ShellKind = "powershell" | "pwsh" | "cmd" | "wsl" | "unix";

export interface ShellProfile {
  id: string;
  label: string;
  /**
   * 내장 프로필의 사전 키(예: "terminal.shell.cmd"). 화면 표시는
   * profileDisplayLabel()이 이 키로 번역한다 — 커스텀 프로필(사용자가
   * 직접 쓴 라벨)은 null이며 원문이 그대로 쓰인다.
   */
  labelKey?: string | null;
  /** 실행할 절대 경로 프로그램. */
  program: string;
  /** program에 붙는 인자 접두어(예: -NoLogo, -d Ubuntu --cd ~). */
  argv: string[];
  kind: ShellKind;
  /** WSL 배포판 이름 등 부가 설명. */
  detail: string | null;
  /** 탐지된 내장 프로필(true)인지 사용자 추가(false)인지. */
  builtin: boolean;
  isDefault?: boolean;
}

/** 화면 표시 라벨: 내장은 labelKey 번역(WSL은 {distro} 치환), 커스텀은 원문. */
export function profileDisplayLabel(
  profile: ShellProfile,
  translate: (key: string, params?: Record<string, string | number>) => string,
): string {
  if (!profile.labelKey) return profile.label;
  return translate(profile.labelKey, { distro: profile.detail ?? "" });
}

/** 프로브 결과: 시스템에 실재하는 셸 후보. */
export interface DetectedShell {
  program: string;
  kind: ShellKind;
  /** WSL 배포판 이름(해당 시에만). */
  distro: string | null;
  isDefault?: boolean;
}

export const POWERSHELL_PATH = "C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe";
export const PWSH_FALLBACK_PATH = "C:\\Program Files\\PowerShell\\7\\pwsh.exe";
export const CMD_PATH = "C:\\Windows\\System32\\cmd.exe";
export const WSL_PATH = "C:\\Windows\\System32\\wsl.exe";

export function wslArgv(distro: string): string[] {
  return distro ? ["-d", distro, "--cd", "~"] : ["--cd", "~"];
}

/**
 * 탐지 결과로 내장 프로필 목록을 만든다. Windows: PowerShell/pwsh/CMD +
 * WSL 배포판 수만큼. macOS/Linux: zsh/bash. 순서가 기본 추천 순서다.
 */
export function builtinProfiles(platform: Platform, detected: DetectedShell[]): ShellProfile[] {
  if (platform === "windows") {
    const first = (kind: ShellKind) => detected.find((d) => d.kind === kind);
    const has = (kind: ShellKind) => first(kind) !== undefined;
    // 탐지가 통째로 비면(프로브 실패·시스템 드라이브가 C:가 아닌 경우 등)
    // 첫 실행 대화상자가 고를 것이 없어 멈춘다 — PowerShell 5와 cmd는
    // 모든 Windows에 있으니 관례 경로로 합성한다. 하나라도 탐지됐으면
    // 탐지 결과를 그대로 믿는다.
    const nothingUsable = !has("powershell") && !has("cmd");
    const powershell = first("powershell") ?? (nothingUsable ? { program: POWERSHELL_PATH } : undefined);
    const cmd = first("cmd") ?? (nothingUsable ? { program: CMD_PATH } : undefined);
    const profiles: ShellProfile[] = [];
    if (powershell) {
      profiles.push({
        id: "powershell",
        label: "PowerShell",
        labelKey: "terminal.shell.powershell",
        program: powershell.program,
        argv: ["-NoLogo"],
        kind: "powershell",
        detail: "Windows PowerShell 5.x",
        builtin: true,
        isDefault: "isDefault" in powershell ? powershell.isDefault : undefined,
      });
    }
    const pwsh = first("pwsh");
    if (pwsh) {
      profiles.push({
        id: "pwsh",
        label: "PowerShell 7",
        labelKey: "terminal.shell.pwsh",
        program: pwsh.program || PWSH_FALLBACK_PATH,
        argv: ["-NoLogo"],
        kind: "pwsh",
        detail: "pwsh",
        builtin: true,
        isDefault: pwsh.isDefault,
      });
    }
    const distros = detected.filter((d) => d.kind === "wsl" && d.distro);
    for (const d of distros) {
      profiles.push({
        id: `wsl:${d.distro}`,
        label: `WSL — ${d.distro}`,
        labelKey: "terminal.shell.wsl",
        program: WSL_PATH,
        argv: wslArgv(d.distro ?? ""),
        kind: "wsl",
        detail: d.distro,
        builtin: true,
      });
    }
    if (cmd) {
      profiles.push({
        id: "cmd",
        label: "명령 프롬프트",
        labelKey: "terminal.shell.cmd",
        program: cmd.program,
        argv: [],
        kind: "cmd",
        detail: "cmd.exe",
        builtin: true,
      });
    }
    return profiles;
  }
  // Use the account's shell path (including Homebrew installs) when detected.
  const unix = detected.filter((shell) => shell.kind === "unix");
  if (unix.length > 0) {
    return unix.map((shell) => {
      const label = shell.program.split("/").pop() ?? shell.program;
      const profile = unixProfile(shell.program, label, platform);
      return {
        ...profile,
        id: shell.program === `/bin/${label}` ? label : `unix:${shell.program}`,
        isDefault: shell.isDefault ?? false,
      };
    }).sort((a, b) => Number(b.isDefault) - Number(a.isDefault));
  }
  return platform === "darwin"
    ? [unixProfile("/bin/zsh", "zsh", platform), unixProfile("/bin/bash", "bash", platform)]
    : [unixProfile("/bin/bash", "bash", platform)];
}

/**
 * macOS/Linux terminals start interactive login shells. On Linux a
 * dock-launched app inherits the systemd user environment, where
 * `~/.profile` (and with it `~/.local/bin`, `~/.cargo/bin`, npm/pnpm
 * prefixes) is not applied under GDM/Wayland — a non-login shell then lacks
 * the very PATH entries the user's CLIs are installed into.
 */
export function defaultShellArgv(platform: Platform): string[] {
  return platform === "darwin" || platform === "linux" ? ["-l", "-i"] : [];
}

function unixProfile(program: string, label: string, platform: Platform): ShellProfile {
  return { id: label, label, labelKey: null, program, argv: ["zsh", "bash"].includes(label) ? defaultShellArgv(platform) : [], kind: "unix", detail: label, builtin: true };
}

/** 기본 프로필 결정: 저장된 id → 플랫폼 관례(Windows PowerShell, macOS zsh, else 첫 항목). */
export function resolveProfile(profiles: ShellProfile[], preferredId: string | null): ShellProfile | null {
  if (profiles.length === 0) return null;
  if (preferredId) {
    const found = profiles.find((p) => p.id === preferredId);
    if (found) return found;
  }
  return (
    profiles.find((p) => p.isDefault) ??
    profiles.find((p) => p.id === "powershell") ??
    profiles.find((p) => p.id === "zsh") ??
    profiles[0]
  );
}

/**
 * 미리보기(모의 데몬)용 프롬프트 결정. 실제 제품의 프롬프트는 실행된 셸이
 * 직접 그린다(PS C:\...>, $ 등) — 이 함수는 모의 셸이 프로필에 맞는
 * 프롬프트를 흉내 내기 위한 것이다.
 */
export function mockPromptFor(program: string, cwd: string): string {
  const kind = shellKindFromProgram(program);
  if (kind === "powershell" || kind === "pwsh") return `PS ${cwd}> `;
  if (kind === "cmd") return `${cwd}> `;
  return "$ ";
}

/** program 경로로 셸 종류 추정(모의 데몬용). */
export function shellKindFromProgram(program: string): ShellKind {
  const base = program.replace(/^.*[/\\]/, "").toLowerCase();
  if (base === "powershell.exe" || base === "powershell") return "powershell";
  if (base === "pwsh.exe" || base === "pwsh") return "pwsh";
  if (base === "cmd.exe" || base === "cmd") return "cmd";
  if (base === "wsl.exe" || base === "wsl") return "wsl";
  return "unix";
}

/** LaunchRequest의 program/argv를 그대로 반영한 프로필(커스텀 추가용). */
export function customProfile(label: string, program: string, argv: string[]): ShellProfile {
  return {
    id: `custom:${label}`,
    label,
    program,
    argv,
    kind: shellKindFromProgram(program),
    detail: null,
    builtin: false,
  };
}
