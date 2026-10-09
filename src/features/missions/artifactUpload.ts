/**
 * 텍스트 artifact 업로드(01-contracts §3 · 05-ui §3/§4).
 *
 * 목표(goal)·메시지 본문처럼 UI가 직접 만드는 artifact는 모두 같은 경로를
 * 지난다: artifact.begin → chunked base64 write →
 * sha256/bytes 검증 commit. chunk 크기는 begin 응답의 `chunk_bytes`를
 * 따른다(계약상 서버가 정한다). 같은 request_id 재시도는 서버 멱등 원장이
 * 같은 upload를 돌려준다.
 */

import type { ArtifactRef } from "../../generated/ArtifactRef";
import type { DaemonClient } from "../daemon/client";
import { bytesToBase64 } from "../daemon/base64";
import { sha256HexAuto } from "../daemon/sha256";

/** 목표 최대 256 KiB UTF-8(05 §3) — 나머지 텍스트에도 같은 상한을 적용한다. */
export const MISSION_TEXT_MAX_BYTES = 256 * 1024;

function newRequestId(): string {
  return typeof crypto !== "undefined" && typeof crypto.randomUUID === "function"
    ? crypto.randomUUID()
    : `req-${Date.now()}-${Math.random().toString(16).slice(2)}`;
}

export interface UploadTextOptions {
  /** 기존 미션의 메시지는 해당 ID에 귀속한다. 새 목표만 null staging. */
  missionId?: string;
  /** 기본 "text/plain; charset=utf-8". */
  mediaType?: string;
  /** 재시도가 같은 멱등 키를 유지할 때 호출부가 지정한다. */
  requestId?: string;
}

/**
 * 텍스트를 지정 미션 또는 staging에 올리고 불변 ref를 돌려준다. bytes 상한을
 * 넘으면 업로드 없이 바로 오류를 낸다(255 KiB를 보내고 퉁치지 않는다).
 */
export async function uploadTextArtifact(
  client: DaemonClient,
  text: string,
  options: UploadTextOptions = {},
): Promise<ArtifactRef> {
  const bytes = new TextEncoder().encode(text);
  if (bytes.byteLength > MISSION_TEXT_MAX_BYTES) {
    throw new Error(`text exceeds ${MISSION_TEXT_MAX_BYTES} bytes`);
  }
  const requestId = options.requestId ?? newRequestId();
  const begin = await client.artifactBegin({
    request_id: requestId,
    mission_id: options.missionId ?? null,
    media_type: options.mediaType ?? "text/plain; charset=utf-8",
    bytes: String(bytes.byteLength),
    sha256: await sha256HexAuto(bytes),
  });
  const chunkBytes = Math.max(1, begin.chunk_bytes);
  for (let offset = 0; offset < bytes.byteLength; offset += chunkBytes) {
    const slice = bytes.subarray(offset, Math.min(offset + chunkBytes, bytes.byteLength));
    await client.artifactWrite({
      upload_id: begin.upload_id,
      offset: String(offset),
      data_b64: bytesToBase64(slice),
    });
  }
  return client.artifactCommit({ upload_id: begin.upload_id });
}
