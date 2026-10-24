/**
 * 설정 → 자원 관리(happy-dom): 데몬이 지금 쓰는 값을 화면 단위(GiB·초·코어)로 보이고,
 * 범위 밖 입력은 보내지 않고 그 자리에 사유를 보인다. 받아들일 값은 컨트롤러로
 * 보내고(저장 형식으로 바꿔), 기본값과 다른 항목만 "기본값으로"를 낸다.
 */

import { afterEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { t } from "../../i18n";
import { ControllerContext } from "../../app/controllerContext";
import type { SessionController } from "../terminal/sessionController";
import type { GuardPolicy } from "../../generated/GuardPolicy";
import { useWorkbenchStore } from "../../store/workbenchStore";
import { RpcClientError } from "../daemon/client";
import { ResourcePanel } from "./ResourcePanel";

const GiB = 1024 ** 3;
const DEFAULTS: GuardPolicy = {
  auto_suspend: true,
  cpu_cores_limit: 6,
  rss_limit_bytes: String(8 * GiB),
  sustain_ms: "20000",
  rss_sustain_ms: "5000",
  auto_resume: false,
};

let root: Root | null = null;
let container: HTMLElement | null = null;

afterEach(() => {
  act(() => root?.unmount());
  root = null;
  container?.remove();
  container = null;
  useWorkbenchStore.setState({
    guardPolicy: null,
    guardPolicyDefault: null,
    reliefPolicy: { auto_yield: true },
    schedulingYield: null,
  });
});

function render(controller: Partial<SessionController> | null): HTMLElement {
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
  act(() => {
    root?.render(
      <ControllerContext.Provider value={controller as SessionController | null}>
        <ResourcePanel />
      </ControllerContext.Provider>,
    );
  });
  return container;
}

const row = (el: HTMLElement, id: string): HTMLElement => el.querySelector<HTMLElement>(`[data-setting-id='${id}']`)!;

function setValue(input: HTMLInputElement, value: string): void {
  const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")?.set;
  act(() => {
    setter?.call(input, value);
    input.dispatchEvent(new Event("input", { bubbles: true }));
  });
}

async function commit(input: HTMLInputElement): Promise<void> {
  await act(async () => {
    input.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));
    for (let i = 0; i < 4; i++) await Promise.resolve();
  });
}

function controller(overrides: Partial<SessionController> = {}) {
  return {
    setGuardPolicy: vi.fn(async (patch: Partial<GuardPolicy>) => {
      const next = { ...useWorkbenchStore.getState().guardPolicy!, ...patch };
      useWorkbenchStore.getState().setGuardPolicy(next);
      return next;
    }),
    resetGuardPolicy: vi.fn(async () => DEFAULTS),
    setReliefAutoYield: vi.fn(async (value: boolean) => ({ auto_yield: value })),
    ...overrides,
  };
}

describe("ResourcePanel", () => {
  it("데몬 값을 화면 단위로 보이고, 기본값과 다른 항목만 되돌리기를 낸다", () => {
    useWorkbenchStore.setState({
      guardPolicy: { ...DEFAULTS, cpu_cores_limit: 4, rss_limit_bytes: String(3 * GiB) },
      guardPolicyDefault: DEFAULTS,
    });
    const fake = controller();
    const el = render(fake);
    expect((row(el, "resourceRssLimit").querySelector("input") as HTMLInputElement).value).toBe("3");
    expect((row(el, "resourceRssSustain").querySelector("input") as HTMLInputElement).value).toBe("5");
    expect((row(el, "resourceCpuLimit").querySelector("input") as HTMLInputElement).value).toBe("4");
    expect((row(el, "resourceCpuSustain").querySelector("input") as HTMLInputElement).value).toBe("20");
    expect(row(el, "resourceCpuLimit").textContent).toContain(t("settings.resetItem"));
    expect(row(el, "resourceCpuSustain").textContent).not.toContain(t("settings.resetItem"));

    const reset = [...row(el, "resourceCpuLimit").querySelectorAll("button")].find(
      (button) => button.textContent === t("settings.resetItem"),
    )!;
    act(() => reset.click());
    expect(fake.resetGuardPolicy).toHaveBeenCalledWith(["cpu_cores_limit"]);
  });

  it("범위 밖 값은 보내지 않고 그 자리에 사유를 보인다", async () => {
    useWorkbenchStore.setState({ guardPolicy: { ...DEFAULTS }, guardPolicyDefault: DEFAULTS });
    const fake = controller();
    const el = render(fake);
    const cpu = row(el, "resourceCpuLimit").querySelector("input") as HTMLInputElement;
    setValue(cpu, "0");
    await commit(cpu);
    expect(fake.setGuardPolicy).not.toHaveBeenCalled();
    expect(row(el, "resourceCpuLimit").querySelector("[role='alert']")?.textContent).toBe(t("settings.resources.error.cpuCores"));

    const memory = row(el, "resourceRssLimit").querySelector("input") as HTMLInputElement;
    setValue(memory, "0.1");
    await commit(memory);
    expect(row(el, "resourceRssLimit").querySelector("[role='alert']")?.textContent).toBe(t("settings.resources.error.rssLimit"));

    const sustain = row(el, "resourceRssSustain").querySelector("input") as HTMLInputElement;
    setValue(sustain, "601");
    await commit(sustain);
    expect(row(el, "resourceRssSustain").querySelector("[role='alert']")?.textContent).toBe(t("settings.resources.error.sustain"));
    expect(fake.setGuardPolicy).not.toHaveBeenCalled();

    // 고친 값은 저장 형식으로 바꿔 보내고 사유를 지운다.
    setValue(cpu, "3");
    await commit(cpu);
    expect(fake.setGuardPolicy).toHaveBeenCalledWith({ cpu_cores_limit: 3 });
    expect(row(el, "resourceCpuLimit").querySelector("[role='alert']")).toBeNull();
    setValue(memory, "2.5");
    await commit(memory);
    expect(fake.setGuardPolicy).toHaveBeenLastCalledWith({ rss_limit_bytes: String(2.5 * GiB) });
    setValue(sustain, "12");
    await commit(sustain);
    expect(fake.setGuardPolicy).toHaveBeenLastCalledWith({ rss_sustain_ms: "12000" });
  });

  it("데몬이 거절하면 그 사유를 항목 아래에 보인다", async () => {
    useWorkbenchStore.setState({ guardPolicy: { ...DEFAULTS }, guardPolicyDefault: DEFAULTS });
    const fake = controller({
      setGuardPolicy: vi.fn(async () => {
        throw new RpcClientError("INVALID_ARGUMENT", "cpu_cores_limit out of range");
      }),
    });
    const el = render(fake);
    const cpu = row(el, "resourceCpuLimit").querySelector("input") as HTMLInputElement;
    setValue(cpu, "8");
    await commit(cpu);
    expect(row(el, "resourceCpuLimit").querySelector("[role='alert']")?.textContent).toBe(
      t("settings.resources.error.failed", { message: "cpu_cores_limit out of range" }),
    );
  });

  it("토글은 곧바로 보내고, 정책을 모르면(연결 전·구 데몬) 안내하며 입력을 막는다", async () => {
    useWorkbenchStore.setState({ guardPolicy: { ...DEFAULTS }, guardPolicyDefault: DEFAULTS });
    const fake = controller();
    const el = render(fake);
    const autoResume = row(el, "resourceAutoResume").querySelector("input") as HTMLInputElement;
    await act(async () => {
      autoResume.click();
      await Promise.resolve();
    });
    expect(fake.setGuardPolicy).toHaveBeenCalledWith({ auto_resume: true });
    const autoYield = row(el, "resourceAutoYield").querySelector("input") as HTMLInputElement;
    await act(async () => {
      autoYield.click();
      await Promise.resolve();
    });
    expect(fake.setReliefAutoYield).toHaveBeenCalledWith(false);

    act(() => root?.unmount());
    container?.remove();
    useWorkbenchStore.setState({ guardPolicy: null });
    const offline = render(controller());
    expect(offline.querySelector("[data-testid='resource-policy-unavailable']")?.textContent).toBe(
      t("settings.resources.unavailable"),
    );
    expect((row(offline, "resourceCpuLimit").querySelector("input") as HTMLInputElement).disabled).toBe(true);
  });
});
