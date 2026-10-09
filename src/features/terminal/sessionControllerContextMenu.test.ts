/**
 * 컨텍스트 메뉴용 controller 표면: 초점 pane이 아니라 leafId로 가리킨 pane의
 * 터미널을 읽고 조작한다(선택·링크·마우스 보고·글꼴·전체 선택·지우기·초점),
 * 값 복사는 결과를 toast로 알린다.
 */

import { afterEach, describe, expect, it, vi } from "vitest";
import type { DaemonClient } from "../daemon/client";
import { SessionController } from "./sessionController";
import { TerminalRegistry, type TerminalLike } from "./registry";
import { setHoveredLink } from "./linkHover";
import { useWorkbenchStore, type PaneMeta } from "../../store/workbenchStore";
import { usePreferences } from "../../store/preferences";
import { t } from "../../i18n";

function fakeTerminal(overrides: Partial<TerminalLike> = {}): TerminalLike {
  return {
    open: () => undefined,
    write: () => undefined,
    resize: () => undefined,
    dispose: () => undefined,
    onData: () => ({ dispose: () => undefined }),
    hasSelection: () => false,
    getSelection: () => "",
    attachCustomKeyEventHandler: () => undefined,
    loadAddon: () => undefined,
    element: null,
    options: { fontSize: 15 },
    ...overrides,
  };
}

function setup(terminal: TerminalLike): SessionController {
  const registry = new TerminalRegistry({
    createTerminal: () => terminal,
    createDom: () => ({ className: "", parentElement: null, remove: () => undefined }),
  });
  registry.acquire("view-1");
  const pane: PaneMeta = {
    leafId: "leaf-1",
    viewId: "view-1",
    sessionId: null,
    workloadId: null,
    title: "zsh",
    cwd: null,
    phase: "live",
    error: null,
    usage: null,
    flowBlocked: false,
  };
  useWorkbenchStore.setState({ panes: { "leaf-1": pane }, focusedLeafId: null });
  return new SessionController({ client: {} as unknown as DaemonClient, registry, platform: "darwin" });
}

afterEach(() => {
  vi.unstubAllGlobals();
  vi.useRealTimers();
  useWorkbenchStore.setState({ panes: {}, focusedLeafId: null, toast: null });
});

describe("SessionController pane context menu surface", () => {
  it("reads selection, hovered link and font size from the addressed pane", () => {
    const terminal = fakeTerminal({ hasSelection: () => true });
    const controller = setup(terminal);
    expect(controller.paneHasSelection("leaf-1")).toBe(true);
    expect(controller.paneFontSize("leaf-1")).toBe(15);

    expect(controller.paneHoveredLink("leaf-1")).toBeNull();
    setHoveredLink(terminal, "https://example.com");
    expect(controller.paneHoveredLink("leaf-1")).toBe("https://example.com");
    setHoveredLink(terminal, null);
    expect(controller.paneHoveredLink("leaf-1")).toBeNull();
  });

  it("falls back safely for an unknown pane", () => {
    const controller = setup(fakeTerminal({ hasSelection: () => true }));
    expect(controller.paneHasSelection("missing")).toBe(false);
    expect(controller.paneHoveredLink("missing")).toBeNull();
    expect(controller.paneMouseTrackingActive("missing")).toBe(false);
    expect(controller.paneFontSize("missing")).toBe(usePreferences.getState().baseFontSize);
  });

  it("treats mouse reporting as app input only when a tracking mode is on", () => {
    expect(setup(fakeTerminal()).paneMouseTrackingActive("leaf-1")).toBe(false);
    expect(setup(fakeTerminal({ modes: { mouseTrackingMode: "none" } })).paneMouseTrackingActive("leaf-1")).toBe(false);
    expect(setup(fakeTerminal({ modes: { mouseTrackingMode: "vt200" } })).paneMouseTrackingActive("leaf-1")).toBe(true);
  });

  it("select all, clear and focus reach the addressed terminal", () => {
    const selectAll = vi.fn();
    const clear = vi.fn();
    const focus = vi.fn();
    const controller = setup(fakeTerminal({ selectAll, clear, focus }));
    controller.selectAllInPane("leaf-1");
    controller.clearPaneScrollback("leaf-1");
    controller.focusPaneTerminal("leaf-1");
    expect(selectAll).toHaveBeenCalledTimes(1);
    expect(clear).toHaveBeenCalledTimes(1);
    expect(focus).toHaveBeenCalledTimes(1);
  });

  it("focus falls back to the terminal's hidden textarea", () => {
    const textareaFocus = vi.fn();
    const element = { querySelector: vi.fn(() => ({ focus: textareaFocus })) };
    const controller = setup(fakeTerminal({ element }));
    controller.focusPaneTerminal("leaf-1");
    expect(element.querySelector).toHaveBeenCalledWith("textarea");
    expect(textareaFocus).toHaveBeenCalledTimes(1);
  });

  it("copyText reports success and failure through the toast", async () => {
    vi.useFakeTimers();
    const writeText = vi.fn(async (_text: string) => undefined);
    vi.stubGlobal("navigator", { clipboard: { writeText } });
    const controller = setup(fakeTerminal());

    await expect(controller.copyText("/work/app")).resolves.toBe(true);
    expect(writeText).toHaveBeenCalledWith("/work/app");
    expect(useWorkbenchStore.getState().toast).toBe(t("terminal.contextMenu.copied"));

    writeText.mockRejectedValueOnce(new Error("denied"));
    await expect(controller.copyText("x")).resolves.toBe(false);
    expect(useWorkbenchStore.getState().toast).toBe(t("terminal.contextMenu.copyFailed"));
  });
});
