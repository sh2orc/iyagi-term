import { beforeEach, expect, it, vi } from "vitest";
import { RateLimitNotice, RetryNotice } from "./RateLimitNotice";
import { click, fakeMission, fakeTask, renderUi, resetAllMissionState } from "./testSupport";
import { t, useI18nStore } from "../../i18n";

beforeEach(resetAllMissionState);

it.each(["ko", "en"] as const)("%s 자동 재시도 시각과 보존되는 실패를 설명한다", language => {
  useI18nStore.setState({ language });
  const after = Date.UTC(2026, 8, 16, 10, 30);
  const task = fakeTask(fakeMission().id, "blocked", { blocked_code: "transient_retry", dispatch_after_unix_ms: String(after) });
  const ui = renderUi(<RetryNotice task={task} />);
  expect(ui.container.textContent).toContain(new Date(after).toLocaleString(language));
  expect(ui.container.textContent).toContain(t("missions.retry.options"));
  expect(ui.container.querySelector("button")).toBeNull();
  ui.unmount();
  for (const changed of [{ ...task, state: "cancelled" as const }, { ...task, blocked_code: "provider_rate_limited" }]) {
    const hidden = renderUi(<RetryNotice task={changed} />);
    expect(hidden.container.textContent).toBe("");
    hidden.unmount();
  }
});

it.each(["ko", "en"] as const)("%s 해제 시각과 이전 실패 복구의 차이를 표시한다", language => {
  useI18nStore.setState({ language });
  const reset = Date.UTC(2026, 8, 16, 10, 30);
  const task = fakeTask(fakeMission().id, "blocked", { blocked_code: "provider_rate_limited", dispatch_after_unix_ms: String(reset) });
  const ui = renderUi(<RateLimitNotice task={task} />);
  expect(ui.container.textContent).toContain(new Date(reset).toLocaleString(language));
  expect(ui.container.textContent).toContain(t("missions.quota.options"));
  expect(ui.container.querySelector("button")).toBeNull();
  ui.unmount();
});

it("종료 작업에는 과거 해제 시각을 활성 대기로 표시하지 않는다", () => {
  const task = fakeTask(fakeMission().id, "cancelled", { blocked_code: "provider_rate_limited", dispatch_after_unix_ms: "1" });
  const ui = renderUi(<RateLimitNotice task={task} />);
  expect(ui.container.textContent).toBe("");
  ui.unmount();
});

it("대기 안내는 한 줄 상태·모델 변경 행동·자세히로 보여 주고 낭독 영역을 만들지 않는다", () => {
  const onChangeModel = vi.fn();
  const reset = Date.UTC(2026, 8, 16, 10, 30);
  const task = fakeTask(fakeMission().id, "blocked", { blocked_code: "provider_rate_limited", dispatch_after_unix_ms: String(reset) });
  const ui = renderUi(<RateLimitNotice task={task} onChangeModel={onChangeModel} />);
  expect(ui.container.querySelector("[data-testid=rate-limit-notice-line]")?.textContent)
    .toBe(t("missions.notice.rateLimit", { reset: new Date(reset).toLocaleString("ko") }));
  expect(ui.container.querySelector("details")?.textContent).toContain(t("missions.quota.options"));
  expect(ui.container.querySelector("[role=status], [aria-live]")).toBeNull();
  click(ui.container.querySelector("[data-testid=rate-limit-change-model]")!);
  expect(onChangeModel).toHaveBeenCalledTimes(1);
  ui.unmount();
});
