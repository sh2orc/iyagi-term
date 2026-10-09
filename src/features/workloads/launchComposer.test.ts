/**
 * 관리 실행 요청 조립: 프로필+폼 → LaunchRequest(생성된 형식과 정확히
 * 일치하는 JSON), interpreter argv 순서, 실행 직전 Windows shim 재검증,
 * argv 예산/예약 하한/env 합성.
 */

import { afterEach, describe, expect, it } from "vitest";
import type { LaunchRequest } from "../../generated/LaunchRequest";
import type { LaunchProfile } from "../profiles/types";
import { defaultProfilePolicy } from "../profiles/types";
import { useI18nStore } from "../../i18n";
import { composeLaunchRequest, type ManagedRunFormInput } from "./launchComposer";

let seq = 0;
const uuid = () => `req-${++seq}`;

function interpreterProfile(): LaunchProfile {
  return {
    id: "p-claude",
    label: "claude",
    descriptor: {
      kind: "claude",
      program: "C:\\Users\\me\\AppData\\Roaming\\npm\\claude.cmd",
      argv_prefix: ["--profile-flag"],
      detected_version: "1.0.7",
      transport: "pty",
      capabilities: { turn_events: false, resume: false, concurrency_control: false },
    },
    cwd: "D:\\work",
    policy: { ...defaultProfilePolicy(), enforcement: "prefer" },
    env: [
      { key: "IYAGI_PROFILE", value: "claude", secretRef: null },
      { key: "ANTHROPIC_API_KEY", value: null, secretRef: "keychain:iyagi/claude" },
    ],
    notes: "",
    interpreter: {
      executable: "C:\\Program Files\\nodejs\\node.exe",
      scriptArgvPrefix: ["C:\\Users\\me\\AppData\\Roaming\\npm\\node_modules\\@anthropic-ai\\claude-code\\cli.js"],
    },
  };
}

function nativeProfile(): LaunchProfile {
  return {
    id: "p-codex",
    label: "codex",
    descriptor: {
      kind: "codex",
      program: "C:\\Tools\\codex\\codex.exe",
      argv_prefix: [],
      detected_version: null,
      transport: "pty",
      capabilities: { turn_events: false, resume: false, concurrency_control: false },
    },
    cwd: "D:\\work",
    policy: defaultProfilePolicy(),
    env: [],
    notes: "",
    interpreter: null,
  };
}

function form(overrides: Partial<ManagedRunFormInput> = {}): ManagedRunFormInput {
  return {
    profileId: "p-claude",
    cwd: "D:\\work",
    extraArgv: ["--verbose"],
    fullAutonomy: false,
    claudeProvider: null,
    priority: 1,
    policy: {
      enforcement: "prefer",
      reservationBytes: 2 * 1024 ** 3,
      cpuSlots: 1,
      memoryMaxBytes: 4 * 1024 ** 3,
      cpuMaxCores: 2,
      pidsMax: 100,
    },
    cols: 80,
    rows: 24,
    ...overrides,
  };
}

describe("composeLaunchRequest — LaunchRequest 정확한 JSON", () => {
  it("interpreter 프로필: program=node.exe, argv=[스크립트, 고정 prefix, 추가 인수]", () => {
    const result = composeLaunchRequest(interpreterProfile(), form(), { uuid, platform: "windows" });
    expect(result.ok).toBe(true);
    if (!result.ok) throw new Error(result.errors.join("\n"));
    // 생성된 형식과 구조가 정확히 일치하는지(타입 + 값).
    const request: LaunchRequest = result.request;
    expect(request).toEqual({
      request_id: "req-1",
      profile_id: "p-claude",
      cwd: "D:\\work",
      program: "C:\\Program Files\\nodejs\\node.exe",
      argv: [
        "C:\\Users\\me\\AppData\\Roaming\\npm\\node_modules\\@anthropic-ai\\claude-code\\cli.js",
        "--profile-flag",
        "--verbose",
      ],
      env_overrides: { IYAGI_PROFILE: "claude" },
      mode: "managed",
        executor: { kind: "local" },
      cols: 80,
      rows: 24,
      priority: 1,
      policy: {
        reservation_bytes: "2147483648",
        cpu_slots: 1,
        enforcement: "prefer",
        memory_max_bytes: "4294967296",
        cpu_max_cores: 2,
        pids_max: 100,
      },
    });
    // secretRef는 env_overrides에 절대 들어가지 않는다.
    expect(Object.keys(request.env_overrides)).not.toContain("ANTHROPIC_API_KEY");
    // 라우팅이 없으면 claude_provider 키 자체가 없다(구 데몬 호환).
    expect("claude_provider" in request).toBe(false);
  });

  it("native 프로필: program을 그대로, prefix 없으면 추가 인수만", () => {
    const result = composeLaunchRequest(
      nativeProfile(),
      form({ profileId: "p-codex", extraArgv: [], policy: { enforcement: "observe", reservationBytes: 2147483648, cpuSlots: 1, memoryMaxBytes: null, cpuMaxCores: null, pidsMax: null } }),
      { uuid, platform: "windows" },
    );
    expect(result.ok).toBe(true);
    if (!result.ok) throw new Error(result.errors.join("\n"));
    expect(result.request.program).toBe("C:\\Tools\\codex\\codex.exe");
    expect(result.request.argv).toEqual([]);
    expect(result.request.policy).toEqual({
      reservation_bytes: "2147483648",
      cpu_slots: 1,
      enforcement: "observe",
      memory_max_bytes: null,
      cpu_max_cores: null,
      pids_max: null,
    });
  });

  it("cwd를 비우면 프로필 기본값을 쓴다", () => {
    const result = composeLaunchRequest(nativeProfile(), form({ profileId: "p-codex", cwd: "  " }), {
      uuid,
      platform: "windows",
    });
    expect(result.ok).toBe(true);
    if (result.ok) expect(result.request.cwd).toBe("D:\\work");
  });

  it("플랫폼이 아니면(=Windows 외) 시스템 심볼릭 링크 프로그램도 허용한다", () => {
    const unixProfile: LaunchProfile = {
      ...nativeProfile(),
      descriptor: { ...nativeProfile().descriptor, program: "/usr/local/bin/claude" },
      cwd: "/home/u/work",
    };
    const result = composeLaunchRequest(unixProfile, form({ profileId: "p-codex", cwd: "/home/u/work" }), {
      uuid,
      platform: "other",
    });
    expect(result.ok).toBe(true);
  });
});

describe("composeLaunchRequest — 실행 직전 차단", () => {
  it("Windows에서 .cmd 직접 실행 프로필은 저장 규칙과 동일하게 거부", () => {
    const shimOnly: LaunchProfile = { ...interpreterProfile(), interpreter: null };
    const result = composeLaunchRequest(shimOnly, form(), { uuid, platform: "windows" });
    expect(result.ok).toBe(false);
    if (!result.ok) {
      expect(result.errors.some((e) => e.includes(".cmd") || e.includes("cmd 셸 shim"))).toBe(true);
      expect(result.errors.some((e) => e.includes("interpreter"))).toBe(true);
    }
  });

  it("argv 개수 상한 초과(프로필 prefix 포함 전체)", () => {
    const many = new Array(300).fill("x");
    const result = composeLaunchRequest(nativeProfile(), form({ profileId: "p-codex", extraArgv: many }), {
      uuid,
      platform: "windows",
    });
    expect(result.ok).toBe(false);
    if (!result.ok) expect(result.errors.some((e) => e.includes("인수가 너무 많습니다"))).toBe(true);
  });

  it("argv 바이트 상한 초과", () => {
    const big = ["x".repeat(65 * 1024)];
    const result = composeLaunchRequest(nativeProfile(), form({ profileId: "p-codex", extraArgv: big }), {
      uuid,
      platform: "windows",
    });
    expect(result.ok).toBe(false);
    if (!result.ok) expect(result.errors.some((e) => e.includes("상한을 넘습니다"))).toBe(true);
  });

  it("예약이 256 MiB 미만이면 거부", () => {
    const result = composeLaunchRequest(
      nativeProfile(),
      form({ profileId: "p-codex", policy: { enforcement: "observe", reservationBytes: 128 * 1024 * 1024, cpuSlots: 1, memoryMaxBytes: null, cpuMaxCores: null, pidsMax: null } }),
      { uuid, platform: "windows" },
    );
    expect(result.ok).toBe(false);
    if (!result.ok) expect(result.errors.some((e) => e.includes("256 MiB"))).toBe(true);
  });

  it("cpu_slots 0이면 거부 / cwd가 상대 경로면 거부", () => {
    const slots = composeLaunchRequest(
      nativeProfile(),
      form({ profileId: "p-codex", policy: { enforcement: "observe", reservationBytes: 2147483648, cpuSlots: 0, memoryMaxBytes: null, cpuMaxCores: null, pidsMax: null } }),
      { uuid, platform: "windows" },
    );
    expect(slots.ok).toBe(false);
    const cwd = composeLaunchRequest(nativeProfile(), form({ profileId: "p-codex", cwd: "relative" }), {
      uuid,
      platform: "windows",
    });
    expect(cwd.ok).toBe(false);
    if (!cwd.ok) expect(cwd.errors.some((e) => e.includes("작업 디렉터리"))).toBe(true);
  });

  it("env 항목에 NUL이 있으면 거부(비밀 참조 항목 자체는 문제 없음)", () => {
    const badEnv: LaunchProfile = {
      ...nativeProfile(),
      env: [{ key: "B\0AD", value: "v", secretRef: null }],
    };
    const result = composeLaunchRequest(badEnv, form({ profileId: "p-codex" }), { uuid, platform: "windows" });
    expect(result.ok).toBe(false);
    if (!result.ok) expect(result.errors.some((e) => e.includes("NUL"))).toBe(true);
  });

  it("모든 오류를 모아서 돌려준다", () => {
    const result = composeLaunchRequest(
      { ...interpreterProfile(), interpreter: null, cwd: "" },
      form({ cwd: "nope", extraArgv: new Array(300).fill("x") }),
      { uuid, platform: "windows" },
    );
    expect(result.ok).toBe(false);
    if (!result.ok) expect(result.errors.length).toBeGreaterThanOrEqual(2);
  });
});

describe("composeLaunchRequest — 검증 문구 i18n", () => {
  afterEach(() => {
    useI18nStore.getState().setLanguage(null);
  });

  it("기본 언어(ko)에서는 한국어 검증 문구가 나온다", () => {
    const result = composeLaunchRequest(
      { ...nativeProfile(), cwd: "" },
      form({ profileId: "p-codex", cwd: "" }),
      { uuid, platform: "windows" },
    );
    expect(result.ok).toBe(false);
    if (!result.ok) expect(result.errors.some((e) => e.includes("작업 디렉터리를 입력하세요"))).toBe(true);
  });

  it("언어를 en으로 바꾸면 영어 검증 문구가 나온다(파라미터 치환 포함)", () => {
    useI18nStore.getState().setLanguage("en");
    const result = composeLaunchRequest(
      { ...nativeProfile(), cwd: "" },
      form({
        profileId: "p-codex",
        cwd: "",
        policy: {
          enforcement: "observe",
          reservationBytes: 128 * 1024 * 1024,
          cpuSlots: 0,
          memoryMaxBytes: null,
          cpuMaxCores: null,
          pidsMax: null,
        },
      }),
      { uuid, platform: "windows" },
    );
    expect(result.ok).toBe(false);
    if (!result.ok) {
      expect(result.errors.some((e) => e.includes("Enter a working directory"))).toBe(true);
      // {minBytes} 파라미터가 치환되었는지(268435456 = 256 MiB).
      expect(result.errors.some((e) => e.includes("at least 256 MiB (268435456 bytes)"))).toBe(true);
      expect(result.errors.some((e) => e.includes("cpu_slots must be an integer"))).toBe(true);
    }
  });
});


describe("composeLaunchRequest autonomy", () => {
  it("defaults to full autonomy when no preference is supplied", () => {
    const result = composeLaunchRequest(nativeProfile(), form({ fullAutonomy: undefined }), { uuid, platform: "windows" });
    expect(result.ok && result.request.argv).toEqual(["--dangerously-bypass-approvals-and-sandbox", "--verbose"]);
  });
  it("unchecked strips flags from both the saved profile and extra launch args", () => {
    const profile = nativeProfile();
    profile.descriptor.argv_prefix = ["--full-auto", "--sandbox", "danger-full-access"];
    const result = composeLaunchRequest(profile, form({ fullAutonomy: false, extraArgv: ["--yolo", "--model", "test"] }), { uuid, platform: "windows" });
    expect(result.ok && result.request.argv).toEqual(["--model", "test"]);
  });
});

describe("composeLaunchRequest — Claude 제공자 라우팅(Z.ai)", () => {
  const zai = { kind: "zai_coding_plan", main_model: "glm-5.3[1m]" } as const;

  it("선택자가 있으면 claude_provider로 실리고, 키·환경 변수는 실리지 않는다", () => {
    const result = composeLaunchRequest(interpreterProfile(), form({ claudeProvider: zai }), {
      uuid,
      platform: "windows",
    });
    expect(result.ok).toBe(true);
    if (!result.ok) throw new Error(result.errors.join("\n"));
    expect(result.request.claude_provider).toEqual({ kind: "zai_coding_plan", main_model: "glm-5.3[1m]" });
    // 프론트는 선택자만 보낸다 — ANTHROPIC_* 환경 변수를 만들지 않는다.
    expect(result.request.env_overrides).toEqual({ IYAGI_PROFILE: "claude" });
    expect(result.request.argv).not.toContain("--model");
  });

  it("secretRef만 있는 ANTHROPIC_API_KEY는 env_overrides에 없으므로 충돌이 아니다(데몬 규칙 미러)", () => {
    const result = composeLaunchRequest(interpreterProfile(), form({ claudeProvider: zai }), {
      uuid,
      platform: "windows",
    });
    expect(result.ok).toBe(true);
  });

  it("라우팅 중 프로필 env에 충돌 키(일반 값)가 있으면 폼 오류로 막는다", () => {
    const conflicting: LaunchProfile = {
      ...nativeProfile(),
      descriptor: { ...nativeProfile().descriptor, kind: "claude" },
      env: [
        { key: "ANTHROPIC_BASE_URL", value: "https://proxy.example", secretRef: null },
        { key: "CLAUDE_CONFIG_DIR", value: "D:\\claude", secretRef: null },
        { key: "CLAUDE_CODE_USE_BEDROCK", value: "1", secretRef: null },
      ],
    };
    const result = composeLaunchRequest(conflicting, form({ profileId: "p-codex", claudeProvider: zai }), {
      uuid,
      platform: "windows",
    });
    expect(result.ok).toBe(false);
    if (!result.ok) {
      const message = result.errors.find((e) => e.includes("ANTHROPIC_BASE_URL"));
      expect(message).toBeDefined();
      expect(message).toContain("CLAUDE_CODE_USE_BEDROCK");
      // CLAUDE_CONFIG_DIR는 허용 — 사용자의 ~/.claude를 그대로 쓴다.
      expect(message).not.toContain("CLAUDE_CONFIG_DIR");
    }
  });

  it("라우팅이 없으면 같은 env라도 막지 않는다", () => {
    const profile: LaunchProfile = {
      ...nativeProfile(),
      env: [{ key: "ANTHROPIC_BASE_URL", value: "https://proxy.example", secretRef: null }],
    };
    const result = composeLaunchRequest(profile, form({ profileId: "p-codex", claudeProvider: null }), {
      uuid,
      platform: "windows",
    });
    expect(result.ok).toBe(true);
    if (result.ok) expect("claude_provider" in result.request).toBe(false);
  });
});
