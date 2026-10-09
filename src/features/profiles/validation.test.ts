/**
 * I11 검증 행렬: Windows 셸 shim 규칙(.cmd/.bat/.ps1 직접 실행 거부,
 * native/interpreter 형태 허용), 플랫폼 주입 판정, argv 예산(256개/64KiB),
 * env NUL 거부, cwd 형식.
 */

import { describe, expect, it } from "vitest";
import type { ProfileInterpreter } from "./types";
import {
  envOverridesForLaunch,
  fileExtension,
  isAbsolutePath,
  isWindowsShellShim,
  normalizeProgramPath,
  parseArgvText,
  validateArgvBudget,
  validateCwd,
  validateEnvEntries,
  validateProgram,
  MAX_ARGV_COUNT,
  MAX_ARGV_TOTAL_BYTES,
} from "./validation";

const interp = (executable: string, prefix: string[] = ["C:\\npm\\node_modules\\x\\cli.js"]): ProfileInterpreter => ({
  executable,
  scriptArgvPrefix: prefix,
});

describe("Windows 셸 shim 검증 (02 §3 마지막 문단)", () => {
  it.each([".cmd", ".bat", ".ps1"] as const)("%s 확장자는 관리 직접 실행이 거부된다", (ext) => {
    const program = `C:\\Users\\me\\AppData\\Roaming\\npm\\claude${ext}`;
    expect(isWindowsShellShim(program)).toBe(true);
    const result = validateProgram({ program, platform: "windows", interpreter: null });
    expect(result.ok).toBe(false);
    if (!result.ok) {
      expect(result.message).toContain(ext.replace(".", ""));
      expect(result.message).toContain("일반 셸"); // 일반 셸 실행 안내
      expect(result.message).toContain("interpreter"); // interpreter+prefix 패턴 안내
      expect(result.message).toContain("node.exe");
      expect(result.message).toContain("cli.js"); // C:\...\claude.js 형태 안내
    }
  });

  it("native .exe는 허용한다", () => {
    expect(validateProgram({ program: "C:\\Tools\\agent.exe", platform: "windows", interpreter: null })).toEqual({
      ok: true,
    });
  });

  it("확장자 없는 절대 경로 프로그램도 형식상 허용한다(unix 대상 등)", () => {
    expect(validateProgram({ program: "C:\\Tools\\agent", platform: "windows", interpreter: null })).toEqual({ ok: true });
  });

  it("interpreter 형태면 shim 프로그램 식별을 허용한다(직접 실행하지 않음)", () => {
    const result = validateProgram({
      program: "C:\\Users\\me\\npm\\claude.cmd",
      platform: "windows",
      interpreter: interp("C:\\Program Files\\nodejs\\node.exe"),
    });
    expect(result).toEqual({ ok: true });
  });

  it("interpreter 실행 파일 자체가 shim이면 거부한다", () => {
    const result = validateProgram({
      program: "C:\\npm\\claude.cmd",
      platform: "windows",
      interpreter: interp("C:\\npm\\node.bat"),
    });
    expect(result.ok).toBe(false);
  });

  it("interpreter 형태는 argv prefix(스크립트 경로)가 필요하다", () => {
    const result = validateProgram({
      program: "C:\\npm\\claude.cmd",
      platform: "windows",
      interpreter: interp("C:\\Program Files\\nodejs\\node.exe", []),
    });
    expect(result.ok).toBe(false);
    if (!result.ok) expect(result.message).toContain("스크립트");
  });

  it("interpreter 실행 파일은 절대 경로여야 한다", () => {
    const result = validateProgram({
      program: "C:\\npm\\claude.cmd",
      platform: "windows",
      interpreter: interp("node.exe"),
    });
    expect(result.ok).toBe(false);
  });

  it("빈 프로그램은 거부한다", () => {
    const result = validateProgram({ program: "  ", platform: "windows", interpreter: null });
    expect(result.ok).toBe(false);
  });
});

describe("플랫폼 주입 판정 (win/other)", () => {
  it("Windows가 아니면 shim 확장자를 문제 삼지 않는다", () => {
    expect(validateProgram({ program: "/opt/cli/claude.cmd", platform: "other", interpreter: null })).toEqual({
      ok: true,
    });
    expect(validateProgram({ program: "/usr/local/bin/claude.ps1", platform: "other", interpreter: null })).toEqual({
      ok: true,
    });
  });

  it("상대 경로는 어느 플랫폼이든 거부한다(정확한 실행 경로 표시 규칙)", () => {
    expect(validateProgram({ program: "claude.cmd", platform: "other", interpreter: null }).ok).toBe(false);
    expect(validateProgram({ program: "bin\\agent", platform: "windows", interpreter: null }).ok).toBe(false);
  });

  it("isAbsolutePath: 드라이브/UNC/유닉스 루트", () => {
    expect(isAbsolutePath("C:\\x\\y", "windows")).toBe(true);
    expect(isAbsolutePath("\\\\server\\share\\x", "windows")).toBe(true);
    expect(isAbsolutePath("/usr/bin/x", "other")).toBe(true);
    expect(isAbsolutePath("C:relative", "windows")).toBe(false);
  });

  it("fileExtension: 마지막 점 이후 소문자, 디렉터리 점은 무시", () => {
    expect(fileExtension("A\\B.EXE")).toBe(".exe");
    expect(fileExtension("C:\\dir.v2\\program")).toBe("");
    expect(fileExtension("C:\\x\\program.cmd?arg")).toBe(".cmd?arg");
  });
});

describe("normalizeProgramPath — 실행될 경로 그대로 표시", () => {
  it("Windows: 슬래시 정규화·중복 제거·끝 구분자 제거(UNC 유지)", () => {
    expect(normalizeProgramPath(" C:/Tools//agent.exe/ ", "windows")).toBe("C:\\Tools\\agent.exe");
    expect(normalizeProgramPath("\\\\\\\\server\\\\share\\\\x.exe\\\\", "windows")).toBe("\\\\server\\share\\x.exe");
  });

  it("unix: 중복 슬래시·끝 슬래시 정리", () => {
    expect(normalizeProgramPath("/usr//local/bin/claude/", "other")).toBe("/usr/local/bin/claude");
  });
});

describe("argv 예산 (01 §4 계약 미러: 256개 / 64 KiB)", () => {
  it("255/256개는 통과, 257개는 거부", () => {
    expect(validateArgvBudget(new Array(255).fill("a")).ok).toBe(true);
    expect(validateArgvBudget(new Array(MAX_ARGV_COUNT).fill("a")).ok).toBe(true);
    expect(validateArgvBudget(new Array(MAX_ARGV_COUNT + 1).fill("a")).countOverflow).toBe(true);
    expect(validateArgvBudget(new Array(MAX_ARGV_COUNT + 1).fill("a")).ok).toBe(false);
  });

  it("정확히 64 KiB는 통과, 1바이트 초과는 거부(UTF-8 바이트 기준)", () => {
    const exact = "x".repeat(MAX_ARGV_TOTAL_BYTES);
    expect(validateArgvBudget([exact]).ok).toBe(true);
    expect(validateArgvBudget(["x".repeat(MAX_ARGV_TOTAL_BYTES + 1)]).bytesOverflow).toBe(true);
    // 한글 1글자 = UTF-8 3바이트
    expect(validateArgvBudget(["한".repeat(MAX_ARGV_TOTAL_BYTES / 3)]).ok).toBe(true);
    expect(validateArgvBudget(["한".repeat(MAX_ARGV_TOTAL_BYTES / 3 + 1)]).ok).toBe(false);
  });

  it("parseArgvText: 줄 단위 인수, 빈 줄 무시, 공백 trim", () => {
    expect(parseArgvText("--verbose\n\n  --model g\n\n")).toEqual(["--verbose", "--model g"]);
  });
});

describe("env allowlist 검증", () => {
  it("NUL 문자가 키/값/참조에 있으면 거부", () => {
    const key = validateEnvEntries([{ key: "BAD\0KEY", value: "v", secretRef: null }]);
    expect(key.ok).toBe(false);
    expect(key.issues[0]?.message).toContain("NUL");
    const value = validateEnvEntries([{ key: "OK", value: "v\0x", secretRef: null }]);
    expect(value.ok).toBe(false);
    expect(value.issues[0]?.message).toContain("NUL");
    const secret = validateEnvEntries([{ key: "OK", value: null, secretRef: "ref\0bad" }]);
    expect(secret.ok).toBe(false);
    expect(secret.issues[0]?.message).toContain("NUL");
  });

  it("중복 키·빈 키·값/참조 미지정 거부", () => {
    const dup = validateEnvEntries([
      { key: "A", value: "1", secretRef: null },
      { key: "A", value: "2", secretRef: null },
    ]);
    expect(dup.ok).toBe(false);
    expect(dup.issues.some((i) => i.message.includes("중복"))).toBe(true);
    expect(validateEnvEntries([{ key: "", value: "v", secretRef: null }]).ok).toBe(false);
    expect(validateEnvEntries([{ key: "X", value: null, secretRef: null }]).ok).toBe(false);
    expect(validateEnvEntries([{ key: "1BAD", value: "v", secretRef: null }]).ok).toBe(false);
  });

  it("정상 항목은 통과하고 env_overrides에는 일반 값만 들어간다(비밀 참조 제외)", () => {
    const entries = [
      { key: "IYAGI_FLAG", value: "1", secretRef: null },
      { key: "API_KEY", value: null, secretRef: "keychain:iyagi/claude" },
    ];
    expect(validateEnvEntries(entries).ok).toBe(true);
    expect(envOverridesForLaunch(entries)).toEqual({ IYAGI_FLAG: "1" });
  });
});

describe("cwd 형식 검증(존재 확인은 daemon — CWD_UNAVAILABLE)", () => {
  it("절대 경로만 허용", () => {
    expect(validateCwd("D:\\work", "windows")).toEqual({ ok: true });
    expect(validateCwd("/home/u/work", "other")).toEqual({ ok: true });
    expect(validateCwd("relative/dir", "other").ok).toBe(false);
    expect(validateCwd("  ", "windows").ok).toBe(false);
  });
});
