/**
 * MissionErrorNotice: role=alert 한 번, 원인 문장, 처리 가능한 행동만 버튼,
 * login은 안내 펼치기, 원문은 `자세히`.
 */

import { beforeEach, expect, it, vi } from "vitest";
import { RpcClientError } from "../daemon/client";
import { t } from "../../i18n";
import { MissionErrorNotice, missionError } from "./errors";
import { click, renderUi, resetAllMissionState } from "./testSupport";

beforeEach(resetAllMissionState);

it("로그인이 필요하면 로그인 방법 안내를 펼쳐 보여 준다", () => {
  const onAction = vi.fn();
  const error = missionError(t, new RpcClientError("AUTH_REQUIRED", "provider says 401"));
  const ui = renderUi(<MissionErrorNotice error={error} onAction={onAction} />);
  expect(ui.container.querySelectorAll("[role=alert]")).toHaveLength(1);
  expect(ui.container.querySelector("[data-testid=mission-error-message]")?.textContent).toBe(t("missions.error.code.authRequired"));
  const button = ui.container.querySelector<HTMLButtonElement>("[data-testid=mission-error-action]")!;
  expect(button.textContent).toBe(t("missions.error.action.login"));
  expect(button.getAttribute("aria-expanded")).toBe("false");
  expect(ui.container.querySelector("[data-testid=mission-error-login]")).toBeNull();
  click(button);
  expect(button.getAttribute("aria-expanded")).toBe("true");
  expect(ui.container.querySelector("[data-testid=mission-error-login]")?.textContent).toContain("codex login");
  expect(ui.container.querySelector("[data-testid=mission-error-login]")?.textContent).toContain("/login");
  expect(onAction).toHaveBeenCalledWith("login");
  expect(ui.container.querySelector("details")?.textContent).toContain("AUTH_REQUIRED: provider says 401");
  ui.unmount();
});

it("다시 시도는 onRetry로, 나머지 행동은 onAction으로 보내고 처리기가 없으면 버튼을 만들지 않는다", () => {
  const onRetry = vi.fn();
  const onAction = vi.fn();
  let ui = renderUi(<MissionErrorNotice error={missionError(t, new RpcClientError("DAEMON_UNAVAILABLE", "x"))} onRetry={onRetry} onAction={onAction} />);
  click(ui.container.querySelector("[data-testid=mission-error-action]")!);
  expect(onRetry).toHaveBeenCalledTimes(1);
  expect(onAction).not.toHaveBeenCalled();
  ui.unmount();

  ui = renderUi(<MissionErrorNotice error={missionError(t, new RpcClientError("NOT_FOUND", "x"))} onAction={onAction} />);
  const button = ui.container.querySelector("[data-testid=mission-error-action]")!;
  expect(button.textContent).toBe(t("missions.error.action.openList"));
  click(button);
  expect(onAction).toHaveBeenCalledWith("open_list");
  ui.unmount();

  ui = renderUi(<MissionErrorNotice error={missionError(t, new RpcClientError("REVISION_CONFLICT", "x"))} />);
  expect(ui.container.querySelector("[data-testid=mission-error-action]")).toBeNull();
  ui.unmount();

  ui = renderUi(<MissionErrorNotice error={missionError(t, undefined)} />);
  expect(ui.container.querySelector("details")).toBeNull();
  expect(ui.container.textContent).toBe(t("missions.error.generic"));
  ui.unmount();
});
