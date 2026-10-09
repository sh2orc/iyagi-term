/**
 * 연동 패널 — Z.ai Coding Plan 카드의 라우팅 스위치 잠금 규칙.
 *
 * 켜는 쪽만 잠근다(키 상태 미확인·키 없음·구 데몬). 이미 켜진 설정은 구 데몬에서도
 * 끌 수 있어야 한다 — 실행 거절 토스트가 "설정 → Z.ai Coding Plan에서 라우팅을
 * 끄세요"라고 안내하기 때문이다. SSR에서는 useEffect가 돌지 않으므로 키 상태는
 * 항상 "확인하는 중"이고, 그 동안에는 사유 문구를 단정하지 않아야 한다.
 */

import { afterEach, describe, expect, it, vi } from "vitest";
import { renderToString } from "react-dom/server";
import { IntegrationPanel } from "./IntegrationPanel";
import { DEFAULT_PREFERENCES, usePreferences } from "../../store/preferences";
import { useWorkbenchStore } from "../../store/workbenchStore";
import { t } from "../../i18n";

const env = vi.hoisted(() => ({ tauri: true }));

// 카드 본문은 Tauri에서만 그린다 — 데스크톱 환경으로 가장한다(IPC 호출은 SSR에서 일어나지 않는다).
vi.mock("../bridge/ipc", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../bridge/ipc")>();
  return { ...actual, isTauri: () => env.tauri };
});

// SSR은 Zustand의 초기 스냅샷을 읽는다 — 테스트가 넣은 현재 상태를 고르게 한다.
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

vi.mock("../../store/workbenchStore", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../store/workbenchStore")>();
  const store = actual.useWorkbenchStore;
  return {
    ...actual,
    useWorkbenchStore: Object.assign(
      (selector: (state: ReturnType<typeof store.getState>) => unknown) => selector(store.getState()),
      store,
    ),
  };
});

afterEach(() => {
  env.tauri = true;
  usePreferences.setState({
    claudeProvider: DEFAULT_PREFERENCES.claudeProvider,
    zaiMainModel: DEFAULT_PREFERENCES.zaiMainModel,
  });
  useWorkbenchStore.setState({ claudeProviderRouting: false });
});

function renderPanel(): string {
  // React SSR는 인접 텍스트 노드 사이에 <!-- --> 마커를 넣는다 — 제거 후 비교.
  return renderToString(<IntegrationPanel />).replace(/<!-- -->/g, "");
}

/** Z.ai 카드의 라우팅 스위치 `<input>` 태그만 집는다(자율 실행 체크박스와 구분). */
function routeSwitch(html: string): string {
  const match = html.match(/<label class="zai-route-switch"[^>]*><input[^>]*>/);
  expect(match).not.toBeNull();
  return match?.[0] ?? "";
}

describe("Z.ai 라우팅 스위치 — 잠금은 켜는 쪽만", () => {
  it("이미 켜진 라우팅은 구 데몬(capability 없음)·키 미확인 상태에서도 끌 수 있다", () => {
    usePreferences.setState({ claudeProvider: "zai-coding-plan" });
    useWorkbenchStore.setState({ claudeProviderRouting: false });
    const input = routeSwitch(renderPanel());
    expect(input).toContain('checked=""');
    expect(input).not.toContain("disabled");
  });

  it("꺼진 라우팅은 키 상태를 확인하기 전까지 켤 수 없다(지원 데몬이어도)", () => {
    useWorkbenchStore.setState({ claudeProviderRouting: true });
    const html = renderPanel();
    const input = routeSwitch(html);
    expect(input).not.toContain('checked=""');
    expect(input).toContain('disabled=""');
    // 확인 중에는 "키를 먼저 등록하라"거나 "구 데몬"이라고 단정하지 않는다.
    expect(html).toContain(t("settings.zai.checking"));
    expect(html).not.toContain(t("settings.zai.route.needsKey"));
    expect(html).not.toContain(t("settings.zai.route.daemonOutdated"));
  });

  it("구 데몬 + 꺼진 라우팅 → 켤 수 없다", () => {
    const input = routeSwitch(renderPanel());
    expect(input).not.toContain('checked=""');
    expect(input).toContain('disabled=""');
  });

  it("주 모델 선택은 라우팅이 켜져 있어도 키 확인 전에는 잠긴다", () => {
    usePreferences.setState({ claudeProvider: "zai-coding-plan" });
    useWorkbenchStore.setState({ claudeProviderRouting: true });
    const html = renderPanel();
    const select = html.match(/<label class="zai-route-model"[^>]*>[\s\S]*?<select[^>]*>/)?.[0] ?? "";
    expect(select).not.toBe("");
    expect(select).toContain('disabled=""');
    expect(html).toContain(t("settings.zai.route.haikuNote", { model: "glm-5.3-flash[1m]" }));
  });

  it("Tauri가 아니면 카드 본문 대신 데스크톱 전용 안내만 낸다", () => {
    env.tauri = false;
    const html = renderPanel();
    expect(html).toContain(t("settings.usage.desktopOnly"));
    expect(html).not.toContain('class="zai-route-switch"');
  });
});
