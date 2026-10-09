/**
 * 종료 확인 모달 렌더: 살아 있는 터미널 수를 문구에 넣고 세 가지 결정
 * (취소·유지·종료)과 "다시 묻지 않기"를 낸다. 상호작용은 컨트롤러 시험
 * (sessionControllerQuit.test.ts)이 맡고 여기서는 props → 화면 매핑만 본다.
 */

import { afterEach, describe, expect, it, vi } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import { ControllerContext } from "./controllerContext";
import { useWorkbenchStore } from "../store/workbenchStore";
import type { SessionController } from "../features/terminal/sessionController";
import { ModalHost } from "./Modals";
import { t } from "../i18n";

// zustand's server snapshot is fixed at store creation; read live state.
vi.mock("../store/workbenchStore", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../store/workbenchStore")>();
  const store = actual.useWorkbenchStore;
  return {
    ...actual,
    useWorkbenchStore: Object.assign(
      (selector: (state: ReturnType<typeof store.getState>) => unknown) => selector(store.getState()),
      store,
    ),
  };
});

function render(): string {
  return renderToStaticMarkup(
    <ControllerContext.Provider value={{} as SessionController}>
      <ModalHost platform="darwin" />
    </ControllerContext.Provider>,
  );
}

afterEach(() => useWorkbenchStore.setState({ modal: null }));

describe("QuitDialog", () => {
  it("shows the live terminal count and the three decisions", () => {
    useWorkbenchStore.setState({ modal: { kind: "quit", sessions: 3 } });
    const html = render();
    expect(html).toContain("quit-dialog");
    expect(html).toContain(t("quit.title"));
    expect(html).toContain(t("quit.description", { n: 3 }));
    expect(html).toContain(t("quit.cancel"));
    expect(html).toContain(t("quit.keep"));
    expect(html).toContain(t("quit.terminate"));
    expect(html).toContain(t("quit.remember"));
    expect(html).not.toContain("quit.");
  });

  it("renders nothing without a modal", () => {
    useWorkbenchStore.setState({ modal: null });
    expect(render()).toBe("");
  });
});
