import { describe, expect, it } from "vitest";
import {
  builtinProfiles,
  mockPromptFor,
  resolveProfile,
  shellKindFromProgram,
  wslArgv,
  type DetectedShell,
} from "./shellProfiles";

const WIN_DETECTED: DetectedShell[] = [
  {
    program: "C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe",
    kind: "powershell",
    distro: null,
  },
  { program: "C:\\Program Files\\PowerShell\\7\\pwsh.exe", kind: "pwsh", distro: null },
  { program: "C:\\Windows\\System32\\wsl.exe", kind: "wsl", distro: "Ubuntu" },
  { program: "C:\\Windows\\System32\\wsl.exe", kind: "wsl", distro: "Debian" },
  { program: "C:\\Windows\\System32\\cmd.exe", kind: "cmd", distro: null },
];

describe("builtinProfiles (Windows)", () => {
  const profiles = builtinProfiles("windows", WIN_DETECTED);

  it("PowerShell/pwsh/WSL 배포판/CMD를 모두 만든다", () => {
    const ids = profiles.map((p) => p.id);
    expect(ids).toContain("powershell");
    expect(ids).toContain("pwsh");
    expect(ids).toContain("wsl:Ubuntu");
    expect(ids).toContain("wsl:Debian");
    expect(ids).toContain("cmd");
  });

  it("WSL 진입 인자는 -d <배포판> --cd ~", () => {
    expect(wslArgv("Ubuntu")).toEqual(["-d", "Ubuntu", "--cd", "~"]);
    const wsl = profiles.find((p) => p.id === "wsl:Ubuntu");
    expect(wsl?.argv).toEqual(["-d", "Ubuntu", "--cd", "~"]);
    expect(wsl?.program).toBe("C:\\Windows\\System32\\wsl.exe");
  });

  it("PowerShell이 최우선 기본 추천", () => {
    expect(resolveProfile(profiles, null)?.id).toBe("powershell");
  });

  it("저장된 기본 프로필 우선", () => {
    expect(resolveProfile(profiles, "wsl:Debian")?.id).toBe("wsl:Debian");
    expect(resolveProfile(profiles, "없는-id")?.id).toBe("powershell");
  });

  it("탐지 안 된 셸은 목록에 없다", () => {
    const only = builtinProfiles("windows", [
      { program: "C:\\Windows\\System32\\cmd.exe", kind: "cmd", distro: null },
    ]);
    expect(only.map((p) => p.id)).toEqual(["cmd"]);
  });

  it("탐지가 통째로 비면 PowerShell 5와 cmd를 관례 경로로 합성해 첫 실행 대화상자가 비지 않는다", () => {
    const fallback = builtinProfiles("windows", []);
    expect(fallback.map((p) => p.id)).toEqual(["powershell", "cmd"]);
    expect(fallback[0].program).toBe("C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe");
    expect(fallback[1].program).toBe("C:\\Windows\\System32\\cmd.exe");
    expect(resolveProfile(fallback, null)?.id).toBe("powershell");
    // WSL만 탐지돼도(PowerShell·cmd 없음) 합성한다.
    const wslOnly = builtinProfiles("windows", [
      { program: "C:\\Windows\\System32\\wsl.exe", kind: "wsl", distro: "Ubuntu" },
    ]);
    expect(wslOnly.map((p) => p.id)).toEqual(["powershell", "wsl:Ubuntu", "cmd"]);
  });

  it("탐지된 경로를 그대로 쓴다 — 시스템 드라이브가 C:가 아니거나 Store pwsh여도 프로필이 생긴다", () => {
    const profiles = builtinProfiles("windows", [
      { program: "D:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe", kind: "powershell", distro: null, isDefault: true },
      { program: "C:\\Users\\u\\AppData\\Local\\Microsoft\\WindowsApps\\pwsh.exe", kind: "pwsh", distro: null },
      { program: "D:\\Windows\\System32\\cmd.exe", kind: "cmd", distro: null },
    ]);
    expect(profiles.find((p) => p.id === "powershell")?.program).toBe("D:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe");
    expect(profiles.find((p) => p.id === "powershell")?.isDefault).toBe(true);
    expect(profiles.find((p) => p.id === "pwsh")?.program).toBe("C:\\Users\\u\\AppData\\Local\\Microsoft\\WindowsApps\\pwsh.exe");
    expect(profiles.find((p) => p.id === "cmd")?.program).toBe("D:\\Windows\\System32\\cmd.exe");
  });
});

describe("builtinProfiles (macOS/Linux)", () => {
  it("prefers the account's Bash over macOS's zsh convention, unless saved explicitly", () => {
    const profiles = builtinProfiles("darwin", [
      { program: "/bin/zsh", kind: "unix", distro: null },
      { program: "/bin/bash", kind: "unix", distro: null, isDefault: true },
    ]);
    expect(profiles[0].program).toBe("/bin/bash");
    expect(resolveProfile(profiles, null)?.program).toBe("/bin/bash");
    expect(resolveProfile(profiles, "zsh")?.program).toBe("/bin/zsh");
  });

  it("keeps a Homebrew login shell's exact path without colliding with /bin/bash", () => {
    const profiles = builtinProfiles("darwin", [
      { program: "/opt/homebrew/bin/bash", kind: "unix", distro: null, isDefault: true },
      { program: "/bin/bash", kind: "unix", distro: null },
    ]);
    expect(resolveProfile(profiles, null)).toMatchObject({
      program: "/opt/homebrew/bin/bash", argv: ["-l", "-i"],
    });
    expect(new Set(profiles.map((p) => p.id)).size).toBe(2);
  });

  it("starts macOS and Linux shells as interactive login shells", () => {
    for (const profile of builtinProfiles("darwin", [])) {
      expect(profile.argv).toEqual(["-l", "-i"]);
    }
    expect(builtinProfiles("linux", [])[0].argv).toEqual(["-l", "-i"]);
  });
  it("macOS는 zsh 우선, linux는 bash 포함", () => {
    const mac = builtinProfiles("darwin", []);
    expect(mac[0]?.id).toBe("zsh");
    const linux = builtinProfiles("linux", [] as DetectedShell[]);
    expect(linux.some((p) => p.id === "bash")).toBe(true);
  });
});

describe("모의 셸 프롬프트(프로필별)", () => {
  it("PowerShell은 PS 프롬프트 — 유닉스 $ 가 아니다", () => {
    expect(
      mockPromptFor(
        "C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe",
        "C:\\Users\\dev",
      ),
    ).toBe("PS C:\\Users\\dev> ");
    expect(mockPromptFor("C:\\Program Files\\PowerShell\\7\\pwsh.exe", "C:\\repo")).toBe(
      "PS C:\\repo> ",
    );
  });

  it("cmd는 경로> 프롬프트, WSL/유닉스는 $", () => {
    expect(mockPromptFor("C:\\Windows\\System32\\cmd.exe", "C:\\repo")).toBe("C:\\repo> ");
    expect(mockPromptFor("C:\\Windows\\System32\\wsl.exe", "/home/dev")).toBe("$ ");
    expect(mockPromptFor("/bin/zsh", "/home/dev")).toBe("$ ");
  });

  it("프로그램 경로로 종류 추정", () => {
    expect(shellKindFromProgram("C:\\x\\POWERSHELL.EXE")).toBe("powershell");
    expect(shellKindFromProgram("/usr/bin/pwsh")).toBe("pwsh");
    expect(shellKindFromProgram("/bin/bash")).toBe("unix");
  });
});
