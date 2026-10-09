/**
 * 클립보드 그림 판정: 형식 고르기, 붙여넣기 이벤트에서 그림 꺼내기,
 * 큰 그림도 넘치지 않는 base64.
 */

import { describe, expect, it } from "vitest";
import {
  bytesToBase64,
  imageExtension,
  imageFileOf,
  pastedText,
  pickImageType,
} from "./clipboardImage";

function item(kind: string, type: string, file: unknown): DataTransferItem {
  return { kind, type, getAsFile: () => file } as unknown as DataTransferItem;
}

function clipboard(options: {
  text?: string;
  items?: DataTransferItem[];
  files?: unknown[];
}): DataTransfer {
  return {
    items: options.items ?? [],
    files: options.files ?? [],
    getData: (type: string) => (type === "text/plain" ? options.text ?? "" : ""),
  } as unknown as DataTransfer;
}

describe("imageExtension", () => {
  it("maps the formats we can save and ignores the rest", () => {
    expect(imageExtension("image/png")).toBe("png");
    expect(imageExtension("image/jpeg")).toBe("jpg");
    expect(imageExtension("image/webp")).toBe("webp");
    expect(imageExtension("text/plain")).toBeNull();
    // 터미널이 경로로 건네도 소용없는 형식(그림 뷰어가 아니라 벡터 문서다).
    expect(imageExtension("image/svg+xml")).toBeNull();
  });

  it("ignores MIME parameters and case", () => {
    expect(imageExtension("IMAGE/PNG")).toBe("png");
    expect(imageExtension("image/png; charset=binary")).toBe("png");
  });
});

describe("pickImageType", () => {
  it("prefers the lossless format when several are offered", () => {
    expect(pickImageType(["image/jpeg", "image/png"])).toBe("image/png");
    expect(pickImageType(["text/html", "image/jpeg"])).toBe("image/jpeg");
  });

  it("is null when nothing on offer is an image we can save", () => {
    expect(pickImageType(["text/plain", "text/html"])).toBeNull();
    expect(pickImageType([])).toBeNull();
  });
});

describe("pastedText", () => {
  it("reads the plain text the event carried", () => {
    expect(pastedText(clipboard({ text: "ls -al" }))).toBe("ls -al");
    expect(pastedText(clipboard({}))).toBe("");
    expect(pastedText(null)).toBe("");
  });
});

describe("imageFileOf", () => {
  it("returns the first image file among the items", () => {
    const png = { type: "image/png" };
    const data = clipboard({
      items: [item("string", "text/html", null), item("file", "image/png", png)],
    });
    expect(imageFileOf(data)).toBe(png);
  });

  it("skips items that are not images and entries with no file behind them", () => {
    const data = clipboard({
      items: [item("file", "application/pdf", { type: "application/pdf" }), item("file", "image/png", null)],
    });
    expect(imageFileOf(data)).toBeNull();
  });

  it("falls back to the file list when the engine reports no items", () => {
    const gif = { type: "image/gif" };
    expect(imageFileOf(clipboard({ files: [{ type: "text/csv" }, gif] }))).toBe(gif);
  });

  it("is null with nothing to paste", () => {
    expect(imageFileOf(null)).toBeNull();
    expect(imageFileOf(clipboard({}))).toBeNull();
  });
});

describe("bytesToBase64", () => {
  it("matches the platform encoder", () => {
    const bytes = Uint8Array.from([0, 1, 2, 250, 251, 255]);
    expect(bytesToBase64(bytes)).toBe(Buffer.from(bytes).toString("base64"));
  });

  it("encodes past the chunk size without blowing the call stack", () => {
    // 32 KiB씩 끊어 넘긴다 — 경계를 여러 번 넘는 크기로 확인한다.
    const bytes = new Uint8Array(200_000).map((_, index) => index % 256);
    expect(bytesToBase64(bytes)).toBe(Buffer.from(bytes).toString("base64"));
  });
});
