/**
 * 관리 실행 대화상자 렌더링: 프로필 선택/인수/정책 UI, direct shell 안내,
 * require + 미지원 → 실행 버튼 비활성 + CAPABILITY_UNAVAILABLE 설명
 * (permission_required 사유 포함), shim 프로필 거부 표시, 호환성 매트릭스,
 * Claude 제공자 라우팅(Z.ai) 안내와 구 데몬 잠금.
 */

import { afterEach, describe, expect, it, vi } from "vitest";
import { renderToString } from "react-dom/server";
import type { DaemonClient } from "../daemon/client";
import type { Capabilities } from "../../generated/Capabilities";
import type { LaunchProfile } from "../profiles/types";
import { defaultProfilePolicy } from "../profiles/types";
import { usePreferences } from "../../store/preferences";
import { t } from "../../i18n";
import { ManagedRunDialog } from "./ManagedRunDialog";
import { formatLaunchError } from "./managedRunErrors";
import { RpcClientError } from "../daemon/client";

// SSR은 Zustand의 초기 스냅샷을 읽는다 — 테스트가 넣은 현재 설정을 고르게 한다.
vi.mock("../../store/preferences", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../store/preferences")>();
  const store = actual.usePreferences;
  return {
    ...actual,
    usePreferences: Object.assign(
      (selector: (state: ReturnType<typeof store.getState>) => unknown) => selector(store.getState()),
      store,
    ),
  };
});

const dummyClient = {} as DaemonClient;

function caps(overrides: Partial<Capabilities> = {}): Capabilities {
  return {
    memory_limit_kind: { support: "unsupported", reason: "이 실행에서는 메모리 상한 적용 불가" },
    cpu_quota: { support: "supported" },
    process_count_limit: { support: "supported" },
    tree_accounting: { support: "unsupported", reason: "mock" },
    reattach: { support: "supported" },
    resume: { support: "unsupported", reason: "R1" },
    scheduling_yield: { support: "supported" },
    suspend_resume: { support: "supported" },
    platform: "windows",
    claude_provider_routing: true,
    ...overrides,
  };
}

function profile(overrides: Partial<LaunchProfile> = {}): LaunchProfile {
  return {
    id: "p1",
    label: "claude",
    descriptor: {
      kind: "claude",
      program: "C:\\Tools\\agent.exe",
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
    ...overrides,
  };
}

function renderDialog(props: Parameters<typeof ManagedRunDialog>[0] = {}, profiles: LaunchProfile[] = [profile()]): string {
  const html = renderToString(
    <ManagedRunDialog client={dummyClient} platform="windows" capabilities={caps()} profiles={profiles} {...props} />,
  );
  // React SSR는 인접 텍스트 노드 사이에 <!-- --> 마커를 넣는다 — 제거 후 비교.
  return html.replace(/<!-- -->/g, "");
}

function submitButton(html: string): string {
  // "실행 등록" 버튼을 정확히 찾는다(폼 안 ProfileForm의 저장 버튼과 구분).
  const match = html.match(/<button[^>]*>실행 등록<\/button>/);
  expect(match).not.toBeNull();
  return match?.[0] ?? "";
}

describe("관리 실행 대화상자 — 기본 구성", () => {
  it("프로필/cwd/인수/정책/우선순위 요소와 안내 문구를 렌더한다", () => {
    const html = renderDialog();
    expect(html).toContain("관리 실행");
    expect(html).toContain("프로필");
    expect(html).toContain("claude — C:\\Tools\\agent.exe");
    expect(html).toContain("작업 디렉터리");
    expect(html).toContain("추가 인수");
    expect(html).toContain("우선순위");
    expect(html).toContain("실행 형태");
    expect(html).toContain("C:\\Tools\\agent.exe");
    expect(html).toContain("1/256개 · 30/65536바이트"); // argv 예산 표시
    expect(html).toContain("PTY/CLI를 미리 만들지 않습니다");
  });

  it("enforcement 세그먼트: 관측만 / 가능하면 적용 / 필수", () => {
    const html = renderDialog();
    expect(html).toContain("관측만");
    expect(html).toContain("가능하면 적용");
    expect(html).toContain("필수");
  });

  it("direct shell 안내 카드(04 §5 attribution 구분)와 호환성 매트릭스", () => {
    const html = renderDialog();
    expect(html).toContain("일반 실행 안내(direct shell)");
    expect(html).toContain("session 단위");
    expect(html).toContain("전용");
    expect(html).toContain(".cmd/.bat/.ps1");
    expect(html).toContain("호환성 매트릭스");
    expect(html).toContain("플랫폼별 자원 적용 범위");
    expect(html).toContain("현재 실행 · Windows");
  });

  it("capability 현실 목록: 요청 전 기본(observe) 상태에서도 지원 상태를 보여 준다", () => {
    const html = renderDialog();
    expect(html).toContain("메모리 상한: 미지원");
    expect(html).toContain("CPU 상한: 적용 가능");
  });

  it("client가 없으면 안내 후 실행 비활성", () => {
    const html = renderDialog({ client: null });
    expect(html).toContain("데몬 연결이 구성되지 않았습니다");
    expect(submitButton(html)).toContain("disabled");
  });
});

describe("require + 미지원 → 실행 차단 (CAPABILITY_UNAVAILABLE)", () => {
  const requireProfile = profile({
    policy: {
      enforcement: "require",
      reservation_bytes: "2147483648",
      cpu_slots: 1,
      memory_max_bytes: "4294967296",
      cpu_max_cores: null,
      pids_max: null,
    },
  });

  it("require + 메모리 상한 요청 + 미지원 → 버튼 비활성 + 설명", () => {
    const html = renderDialog({}, [requireProfile]);
    expect(submitButton(html)).toContain("disabled");
    expect(html).toContain("CAPABILITY_UNAVAILABLE");
    expect(html).toContain("메모리 상한");
    expect(html).toContain("이 실행에서는 메모리 상한 적용 불가");
  });

  it("permission_required 사유도 표면에 노출된다", () => {
    const html = renderDialog(
      {
        capabilities: caps({ memory_limit_kind: { support: "permission_required", reason: "Job 권한 없음" } }),
      },
      [requireProfile],
    );
    expect(submitButton(html)).toContain("disabled");
    expect(html).toContain("권한 필요");
    expect(html).toContain("Job 권한 없음");
  });

  it("같은 정책이라도 observe/prefer이면 비활성화되지 않는다", () => {
    const preferProfile = profile({
      policy: {
        enforcement: "prefer",
        reservation_bytes: "2147483648",
        cpu_slots: 1,
        memory_max_bytes: "4294967296",
        cpu_max_cores: null,
        pids_max: null,
      },
    });
    const html = renderDialog({}, [preferProfile]);
    expect(submitButton(html)).not.toContain("disabled");
    expect(html).toContain("누락"); // prefer + 미지원 → 누락 알림
  });
});

describe("프로필 프로그램 규칙이 대화상자에 반영된다", () => {
  it("Windows .cmd 직접 실행 프로필은 실행이 막히고 행동 지시가 보인다", () => {
    const html = renderDialog({}, [profile({ descriptor: { ...profile().descriptor, program: "C:\\npm\\claude.cmd" } })]);
    expect(submitButton(html)).toContain("disabled");
    expect(html).toContain("직접 실행할 수 없습니다");
    expect(html).toContain("interpreter");
  });

  it("interpreter 형태 프로필은 고정 prefix가 인수 편집기에 표시된다", () => {
    const interp = profile({
      descriptor: {
        ...profile().descriptor,
        program: "C:\\npm\\claude.cmd",
        argv_prefix: ["--flag"],
      },
      interpreter: {
        executable: "C:\\Program Files\\nodejs\\node.exe",
        scriptArgvPrefix: ["C:\\npm\\node_modules\\cli.js"],
      },
    });
    const html = renderDialog({}, [interp]);
    expect(submitButton(html)).not.toContain("disabled");
    expect(html).toContain("프로필 고정 prefix(변경 불가)");
    expect(html).toContain("C:\\npm\\node_modules\\cli.js");
    expect(html).toContain("--flag");
    expect(html).toContain("C:\\Program Files\\nodejs\\node.exe");
  });

  it("프로그램 미설정 프로필은 비활성화된다", () => {
    const html = renderDialog({}, [profile({ descriptor: { ...profile().descriptor, program: "" } })]);
    expect(html).toContain("프로그램 미설정");
    expect(submitButton(html)).toContain("disabled");
  });
});

describe("Claude 제공자 라우팅(Z.ai) — 대화상자 자체 관문", () => {
  const routedLine = (model: string) => t("managed.claudeProviderRouted", { model });
  const outdated = () => t("settings.zai.launch.daemonOutdated");

  afterEach(() => usePreferences.setState({ claudeProvider: "anthropic", zaiMainModel: "glm-5.3[1m]" }));

  it("라우팅 켜짐 + 구 데몬(capability 없음) → 실행 잠금 + daemonOutdated 안내", () => {
    usePreferences.setState({ claudeProvider: "zai-coding-plan" });
    const html = renderDialog({ capabilities: caps({ claude_provider_routing: false }) });
    expect(html).toContain(outdated());
    expect(html).not.toContain(routedLine("glm-5.3[1m]"));
    expect(submitButton(html)).toContain("disabled");
  });

  it("라우팅 켜짐 + 지원 데몬 → 주 모델과 함께 라우팅 안내, 실행은 열린다", () => {
    usePreferences.setState({ claudeProvider: "zai-coding-plan", zaiMainModel: "glm-5.3[1m]" });
    const html = renderDialog();
    expect(html).toContain(routedLine("glm-5.3[1m]"));
    expect(html).not.toContain(outdated());
    expect(submitButton(html)).not.toContain("disabled");
  });

  it("선택한 주 모델이 안내 문구에 반영된다", () => {
    usePreferences.setState({ claudeProvider: "zai-coding-plan", zaiMainModel: "glm-5.3-flash[1m]" });
    const html = renderDialog();
    expect(html).toContain(routedLine("glm-5.3-flash[1m]"));
    expect(html).not.toContain(routedLine("glm-5.3[1m]"));
  });

  it("codex 프로필은 설정이 켜져 있어도 라우팅되지 않는다(안내·잠금 없음)", () => {
    usePreferences.setState({ claudeProvider: "zai-coding-plan" });
    const codex = profile({ label: "codex", descriptor: { ...profile().descriptor, kind: "codex" } });
    const html = renderDialog({ capabilities: caps({ claude_provider_routing: false }) }, [codex]);
    expect(html).not.toContain(outdated());
    expect(html).not.toContain(routedLine("glm-5.3[1m]"));
    expect(submitButton(html)).not.toContain("disabled");
  });

  it("설정이 Anthropic(기본)이면 Claude 프로필에도 라우팅 문구가 없다", () => {
    const html = renderDialog();
    expect(html).not.toContain(outdated());
    expect(html).not.toContain(routedLine("glm-5.3[1m]"));
    expect(submitButton(html)).not.toContain("disabled");
  });
});

describe("formatLaunchError — daemon 오류 문구", () => {
  it("CAPABILITY_UNAVAILABLE은 설명 문구로 변환된다", () => {
    const message = formatLaunchError(
      new RpcClientError("CAPABILITY_UNAVAILABLE", "memory_max_bytes 미지원", false),
    );
    expect(message).toContain("실행 불가");
    expect(message).toContain("CAPABILITY_UNAVAILABLE");
    expect(message).toContain("memory_max_bytes");
  });

  it("다른 코드는 코드 라벨과 함께 표시된다", () => {
    const message = formatLaunchError(new RpcClientError("QUEUE_FULL", "대기열 가득 참", false));
    expect(message).toContain("QUEUE_FULL");
    expect(message).toContain("대기열 가득 참");
  });
});
