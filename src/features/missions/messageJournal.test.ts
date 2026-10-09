import { expect, it, vi } from "vitest";
import { decodePendingMessage, messageScope, readPendingMessageBody } from "./messageJournal";
import { bytesToBase64 } from "../daemon/base64";
import { sha256HexAuto } from "../daemon/sha256";
import type { MissionMessageParams } from "../../generated/MissionMessageParams";

const params: MissionMessageParams = { request_id: "request-1", mission_id: "mission-1", expected_revision: "9223372036854775806",
  target_task_id: "task-1", body_ref: { id: "artifact-1", bytes: "12", sha256: "0".repeat(64), media_type: "text/plain; charset=utf-8" } };
const scope = messageScope(params.mission_id, params.target_task_id);

it("normal/대체 요청의 null·생략 필드와 큰 revision을 그대로 복원한다", () => {
  for (const p of [params, { ...params, supersedes_message_id: null }, { ...params, supersedes_message_id: "source-1" }]) {
    expect(decodePendingMessage({ version: 1, params: p }, messageScope(p.mission_id, p.target_task_id, p.supersedes_message_id ?? null))).toEqual(p);
  }
});

it.each([
  { version: 2, params },
  { version: 1, params, extra: "future field" },
  { version: 1, params: { ...params, extra: "future field" } },
  { version: 1, params: { ...params, mission_id: "other-mission" } },
  { version: 1, params: { ...params, target_task_id: null } },
  { version: 1, params: { ...params, request_id: "bad\nid" } },
  { version: 1, params: { ...params, expected_revision: "9223372036854775808" } },
  { version: 1, params: { ...params, expected_revision: "01" } },
  { version: 1, params: { ...params, expected_revision: 1 } },
  { version: 1, params: { ...params, body_ref: { ...params.body_ref, bytes: "262145" } } },
  { version: 1, params: { ...params, body_ref: { ...params.body_ref, sha256: "not-a-hash" } } },
  { version: 1, params: { ...params, body_ref: { ...params.body_ref, media_type: "text/html" } } },
  { version: 1, params: { ...params, body_ref: { ...params.body_ref, text: "must not be stored" } } },
  { version: 1, params: { ...params, supersedes_message_id: "source-1" } },
])("손상·다른 scope·미지원 필드를 거절하고 전송을 진행하지 않는다: %#", value => {
  expect(() => decodePendingMessage(value, scope)).toThrow();
});

it("여러 페이지에 걸친 UTF-8 본문을 길이와 SHA-256까지 검사한다", async () => {
  const bytes = new TextEncoder().encode("한글 😀 줄바꿈\n".repeat(450));
  const ref = { ...params.body_ref, bytes: String(bytes.length), sha256: await sha256HexAuto(bytes) };
  const read = vi.fn(async ({ offset }: { offset: string }) => {
    const start = Number(offset), end = Math.min(start + 4093, bytes.length);
    return { data_b64: bytesToBase64(bytes.subarray(start, end)), next_offset: String(end), complete: end === bytes.length };
  });
  expect(await readPendingMessageBody({ artifactRead: read }, ref)).toBe(new TextDecoder().decode(bytes));
  expect(read.mock.calls.length).toBeGreaterThan(1);
});

it.each([
  { data_b64: "", next_offset: "0", complete: false },
  { data_b64: "YQ==", next_offset: "0", complete: true },
  { data_b64: "YQ==", next_offset: "01", complete: true },
  { data_b64: "YQ==", next_offset: "1", complete: true },
  { data_b64: "YWI=", next_offset: "2", complete: false },
  { data_b64: "YWJj", next_offset: "3", complete: true },
  { data_b64: "a".repeat(5465), next_offset: "2", complete: true },
])("전진하지 않는 cursor·잘린 본문·초과 페이지는 복원하지 않는다: %#", async page => {
  const ref = { ...params.body_ref, bytes: "2", sha256: await sha256HexAuto(new TextEncoder().encode("ab")) };
  await expect(readPendingMessageBody({ artifactRead: vi.fn().mockResolvedValue(page) }, ref)).rejects.toThrow();
});

it("해시가 맞아도 잘못된 UTF-8은 표시하지 않는다", async () => {
  const bytes = new Uint8Array([0xff]);
  const ref = { ...params.body_ref, bytes: "1", sha256: await sha256HexAuto(bytes) };
  await expect(readPendingMessageBody({ artifactRead: vi.fn().mockResolvedValue({ data_b64: bytesToBase64(bytes), next_offset: "1", complete: true }) }, ref)).rejects.toThrow();
});
