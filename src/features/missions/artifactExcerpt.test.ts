import { describe, expect, it, vi } from "vitest";
import type { ArtifactReadParams } from "../../generated/ArtifactReadParams";
import { readArtifactExcerpt } from "./artifactExcerpt";

/** artifact.read 계약(페이지 ≤ 4 KiB, next_offset/complete)을 흉내 낸다. */
function fakeClient(bytes: Uint8Array) {
  const artifactRead = vi.fn(async (params: ArtifactReadParams) => {
    const offset = Number(params.offset);
    const end = Math.min(offset + Math.min(params.max_bytes, 4096), bytes.byteLength);
    let binary = "";
    for (const byte of bytes.subarray(offset, end)) binary += String.fromCharCode(byte);
    return { data_b64: btoa(binary), next_offset: String(end), complete: end >= bytes.byteLength };
  });
  return { artifactRead };
}

function ref(bytes: Uint8Array) {
  return { id: "log", sha256: "0".repeat(64), bytes: String(bytes.byteLength), media_type: "text/plain" };
}

describe("readArtifactExcerpt", () => {
  it("작은 로그는 전체를 읽는다", async () => {
    const body = new TextEncoder().encode("iyagi verification log\nexit_code: 0\n");
    const client = fakeClient(body);
    const excerpt = await readArtifactExcerpt(client, ref(body));
    expect(excerpt).toEqual({
      head: "iyagi verification log\nexit_code: 0\n",
      tail: null,
      totalBytes: body.byteLength,
      shownHeadBytes: body.byteLength,
      shownTailBytes: 0,
    });
  });

  it("큰 로그는 앞·뒤 일부만 읽고 중간 페이지는 요청하지 않는다", async () => {
    const body = new TextEncoder().encode("H".repeat(10_000) + "M".repeat(50_000) + "T".repeat(10_000));
    const client = fakeClient(body);
    const excerpt = await readArtifactExcerpt(client, ref(body), { fullMaxBytes: 20_000, edgeBytes: 8_000 });
    expect(excerpt.head).toBe("H".repeat(8_000));
    expect(excerpt.tail).toBe("T".repeat(8_000));
    expect(excerpt.totalBytes).toBe(70_000);
    const offsets = client.artifactRead.mock.calls.map(([params]) => Number(params.offset));
    expect(offsets.every((offset) => offset < 8_000 || offset >= 62_000)).toBe(true);
    expect(client.artifactRead.mock.calls.every(([params]) => params.max_bytes <= 4096)).toBe(true);
  });

  it("뒷부분이 UTF-8 문자 중간에서 시작하면 다음 문자 경계부터 보여 준다", async () => {
    const body = new TextEncoder().encode("a".repeat(100) + "가나다라");
    const client = fakeClient(body);
    // 마지막 7바이트 = "나"의 마지막 1바이트 + "다라"(6바이트)
    const excerpt = await readArtifactExcerpt(client, ref(body), { fullMaxBytes: 50, edgeBytes: 7 });
    expect(excerpt.tail).toBe("다라");
    expect(excerpt.shownTailBytes).toBe(6);
  });
});
