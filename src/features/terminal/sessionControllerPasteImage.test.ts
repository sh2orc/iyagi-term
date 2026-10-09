/**
 * 그림 붙여넣기·파일 떨구기(controller 쪽): 그림은 임시 파일 포트로 보내고
 * 돌아온 경로를 셸이 읽을 수 있는 꼴로 붙여넣는다. 포트가 없으면 아무것도
 * 하지 않고 글 경로로 되돌아가고, 상한 초과·저장 실패는 toast로 알린다.
 */

import { afterEach, describe, expect, it } from "vitest";
import type { DaemonClient } from "../daemon/client";
import { SessionController } from "./sessionController";
import { TerminalRegistry } from "./registry";
import { PASTE_IMAGE_MAX_BYTES, type PasteImageSource } from "./clipboardImage";
import type { PasteImagePort } from "./pasteImage";
import type { Platform } from "./shortcuts";
import { useWorkbenchStore } from "../../store/workbenchStore";
import { t } from "../../i18n";

function image(bytes: Uint8Array, type = "image/png"): PasteImageSource {
  return { type, arrayBuffer: async () => bytes.slice().buffer };
}

interface Harness {
  controller: SessionController;
  pasted: Array<{ text: string; leafId?: string }>;
  saved: Array<{ bytes: Uint8Array; mime: string }>;
}

function setup(options: { platform?: Platform; save?: PasteImagePort["save"] } = {}): Harness {
  const saved: Harness["saved"] = [];
  const port: PasteImagePort | undefined = options.save
    ? {
        save: (bytes, mime) => {
          saved.push({ bytes, mime });
          return options.save!(bytes, mime);
        },
      }
    : undefined;
  const controller = new SessionController({
    client: {} as unknown as DaemonClient,
    registry: new TerminalRegistry({
      createTerminal: () => {
        throw new Error("these tests never open a terminal");
      },
      createDom: () => ({ className: "", parentElement: null, remove: () => undefined }),
    }),
    platform: options.platform ?? "linux",
    savePasteImage: port,
  });
  // 붙여넣기 페이로드만 본다 — bracketed paste·큐 규칙은 sendPaste의 몫이고
  // 여기서 확인할 것은 "무엇을 붙여넣기로 넘겼는가"다.
  const pasted: Harness["pasted"] = [];
  controller.pasteText = (text: string, leafId?: string) => {
    pasted.push({ text, leafId });
  };
  return { controller, pasted, saved };
}

afterEach(() => {
  useWorkbenchStore.setState({ panes: {}, focusedLeafId: null, toast: null });
});

describe("SessionController.pasteImage", () => {
  it("saves the bytes and pastes the path it got back", async () => {
    const harness = setup({ save: async () => "/tmp/iyagi/paste/iyagi-paste-1-ab.png" });
    const bytes = Uint8Array.from([137, 80, 78, 71]);

    await expect(harness.controller.pasteImage(image(bytes), "leaf-1")).resolves.toBe(true);

    expect(harness.saved).toHaveLength(1);
    expect(Array.from(harness.saved[0]!.bytes)).toEqual([137, 80, 78, 71]);
    expect(harness.saved[0]!.mime).toBe("image/png");
    expect(harness.pasted).toEqual([
      { text: "/tmp/iyagi/paste/iyagi-paste-1-ab.png", leafId: "leaf-1" },
    ]);
  });

  it("quotes a saved path the shell would otherwise split", async () => {
    const path = String.raw`C:\Users\a b\AppData\Local\Temp\iyagi-paste-1-ab.png`;
    const harness = setup({ platform: "windows", save: async () => path });

    await harness.controller.pasteImage(image(Uint8Array.from([1])), "leaf-1");

    expect(harness.pasted[0]!.text).toBe(`"${path}"`);
  });

  it("does nothing when the app cannot save files (browser preview)", async () => {
    const harness = setup();
    await expect(harness.controller.pasteImage(image(Uint8Array.from([1])))).resolves.toBe(false);
    expect(harness.pasted).toEqual([]);
  });

  it("refuses an image past the paste limit and says so", async () => {
    const harness = setup({ save: async () => "/tmp/never.png" });

    const oversize = new Uint8Array(PASTE_IMAGE_MAX_BYTES + 1);
    await expect(harness.controller.pasteImage(image(oversize))).resolves.toBe(true);

    expect(harness.saved).toEqual([]);
    expect(harness.pasted).toEqual([]);
    expect(useWorkbenchStore.getState().toast).toBe(
      t("terminal.paste.imageTooLarge", { max: PASTE_IMAGE_MAX_BYTES / (1024 * 1024) }),
    );
  });

  it("reports a failed save through the toast and pastes nothing", async () => {
    const harness = setup({
      save: async () => {
        throw new Error("disk full");
      },
    });

    await expect(harness.controller.pasteImage(image(Uint8Array.from([1])))).resolves.toBe(true);

    expect(harness.pasted).toEqual([]);
    expect(useWorkbenchStore.getState().toast).toBe(t("terminal.paste.imageFailed"));
  });

  it("skips an empty image instead of writing a zero-byte file", async () => {
    const harness = setup({ save: async () => "/tmp/never.png" });
    await expect(harness.controller.pasteImage(image(new Uint8Array(0)))).resolves.toBe(false);
    expect(harness.saved).toEqual([]);
  });
});

describe("SessionController.pasteDroppedPaths", () => {
  it("pastes every dropped path into the pane under the cursor and focuses it", () => {
    const harness = setup();

    harness.controller.pasteDroppedPaths(["/a/shot.png", "/a/build log.txt"], "leaf-2");

    expect(harness.pasted).toEqual([
      { text: "/a/shot.png '/a/build log.txt'", leafId: "leaf-2" },
    ]);
    expect(useWorkbenchStore.getState().focusedLeafId).toBe("leaf-2");
  });

  it("leaves the pane alone when the drop carried nothing usable", () => {
    const harness = setup();
    harness.controller.pasteDroppedPaths([], "leaf-2");
    expect(harness.pasted).toEqual([]);
    expect(useWorkbenchStore.getState().focusedLeafId).toBeNull();
  });
});
