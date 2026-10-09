/**
 * 큰 artifact(검증 로그 등)의 제한 읽기.
 *
 * `readArtifactText`는 본문 전체를 읽고 화면 수명 캐시에 넣는다 — 로그처럼
 * 커질 수 있는 본문에는 맞지 않는다. 여기서는 작은 본문은 전체를, 큰 본문은
 * 앞/뒤 일부만 `artifact.read`(페이지 4 KiB 이하)로 읽고 캐시하지 않는다.
 */

import { useEffect, useState } from "react";
import type { ArtifactRef } from "../../generated/ArtifactRef";
import type { DaemonClient } from "../daemon/client";
import { getMissionClient } from "./clientAccess";

/** 이 크기 이하는 전체를 보여 준다. */
export const EXCERPT_FULL_MAX_BYTES = 64 * 1024;
/** 큰 본문에서 앞·뒤 각각 보여 줄 크기. */
export const EXCERPT_EDGE_BYTES = 16 * 1024;
const PAGE_BYTES = 4096;

export interface ArtifactExcerpt {
  head: string;
  /** 잘린 경우의 뒷부분(전체를 읽었으면 null). */
  tail: string | null;
  totalBytes: number;
  shownHeadBytes: number;
  shownTailBytes: number;
}

function decodeBase64(b64: string): Uint8Array {
  const binary = atob(b64);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i++) bytes[i] = binary.charCodeAt(i);
  return bytes;
}

async function readRange(
  client: Pick<DaemonClient, "artifactRead">,
  artifactId: string,
  offset: number,
  length: number,
): Promise<Uint8Array> {
  const chunks: Uint8Array[] = [];
  let position = offset;
  const end = offset + length;
  let total = 0;
  while (position < end) {
    const page = await client.artifactRead({
      artifact_id: artifactId,
      offset: String(position),
      max_bytes: Math.min(PAGE_BYTES, end - position),
    });
    const bytes = decodeBase64(page.data_b64);
    const next = Number(page.next_offset);
    if (bytes.byteLength > 0) {
      chunks.push(bytes);
      total += bytes.byteLength;
    }
    if (page.complete) break;
    if (!Number.isSafeInteger(next) || next <= position) throw new Error("Artifact cursor did not advance");
    position = next;
  }
  const joined = new Uint8Array(Math.min(total, length));
  let cursor = 0;
  for (const chunk of chunks) {
    const take = Math.min(chunk.byteLength, joined.byteLength - cursor);
    joined.set(chunk.subarray(0, take), cursor);
    cursor += take;
    if (cursor >= joined.byteLength) break;
  }
  return joined;
}

/** UTF-8 연속 바이트(10xxxxxx)로 시작하는 뒷부분은 첫 문자 경계까지 건너뛴다. */
function skipContinuation(bytes: Uint8Array): Uint8Array {
  let start = 0;
  while (start < bytes.byteLength && start < 3 && (bytes[start] & 0xc0) === 0x80) start += 1;
  return bytes.subarray(start);
}

export async function readArtifactExcerpt(
  client: Pick<DaemonClient, "artifactRead">,
  ref: ArtifactRef,
  limits: { fullMaxBytes?: number; edgeBytes?: number } = {},
): Promise<ArtifactExcerpt> {
  const fullMax = limits.fullMaxBytes ?? EXCERPT_FULL_MAX_BYTES;
  const edge = limits.edgeBytes ?? EXCERPT_EDGE_BYTES;
  const totalBytes = Number(ref.bytes);
  if (!Number.isSafeInteger(totalBytes) || totalBytes < 0) throw new Error("Invalid artifact size");
  const decoder = () => new TextDecoder("utf-8", { fatal: false });
  if (totalBytes <= fullMax) {
    const bytes = await readRange(client, ref.id, 0, totalBytes);
    return { head: decoder().decode(bytes), tail: null, totalBytes, shownHeadBytes: bytes.byteLength, shownTailBytes: 0 };
  }
  const head = await readRange(client, ref.id, 0, edge);
  const tail = skipContinuation(await readRange(client, ref.id, totalBytes - edge, edge));
  return {
    head: decoder().decode(head),
    tail: decoder().decode(tail),
    totalBytes,
    shownHeadBytes: head.byteLength,
    shownTailBytes: tail.byteLength,
  };
}

export interface ArtifactExcerptState {
  excerpt: ArtifactExcerpt | null;
  error: boolean;
}

/** ref가 null이면 읽지 않는다(접힌 로그는 펼칠 때만 읽는다). */
export function useArtifactExcerpt(ref: ArtifactRef | null): ArtifactExcerptState {
  const artifactId = ref?.id ?? null;
  const [state, setState] = useState<ArtifactExcerptState>({ excerpt: null, error: false });
  useEffect(() => {
    if (!ref) {
      setState({ excerpt: null, error: false });
      return;
    }
    const client = getMissionClient();
    if (!client) {
      setState({ excerpt: null, error: true });
      return;
    }
    let alive = true;
    setState({ excerpt: null, error: false });
    readArtifactExcerpt(client, ref)
      .then((excerpt) => {
        if (alive) setState({ excerpt, error: false });
      })
      .catch(() => {
        if (alive) setState({ excerpt: null, error: true });
      });
    return () => {
      alive = false;
    };
    // artifact id만 감시한다 — 같은 artifact의 bytes는 바뀌지 않는다.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [artifactId]);
  return state;
}
