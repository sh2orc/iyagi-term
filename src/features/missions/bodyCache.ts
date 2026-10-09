/**
 * artifact 본문 읽기 캐시(05-ui §4 · O16).
 *
 * Lead 대화·질문 문구처럼 Message/Decision의 body_ref를 화면이 다시 읽을
 * 때 artifact.read를 매 렌더마다 부르지 않게 한다. 단순 Map LRU + 바이트
 * 계정으로 8 MiB 상한을 지킨다 — 대화 원문은 store에 두지 않는 계약
 * (05 §2)을 깨지 않는, 화면 수명과 같은 캐시다.
 */

import type { DaemonClient } from "../daemon/client";

/** 캐시 상한(바이트). 본문 문자열 길이를 근사 바이트로 계산한다. */
export const BODY_CACHE_MAX_BYTES = 8 * 1024 * 1024;

interface CacheEntry {
  text: string;
  /** 근사 바이트(UTF-16 코드 단위 → 최악 3배 UTF-8 근사는 아니고 2배). */
  bytes: number;
}

const BYTE_SIZE_LATIN1_MAX = 1;
const BYTE_SIZE_OTHER = 3;

function approximateBytes(text: string): number {
  let total = 0;
  for (let i = 0; i < text.length; i++) {
    total += text.charCodeAt(i) <= 0xff ? BYTE_SIZE_LATIN1_MAX : BYTE_SIZE_OTHER;
  }
  return total;
}

const cache = new Map<string, CacheEntry>();
let totalBytes = 0;

function evictIfNeeded(): void {
  // Map은 삽입 순서대로 순회되므로 첫 원소가 가장 오래된 것이다(LRU).
  while (totalBytes > BODY_CACHE_MAX_BYTES && cache.size > 1) {
    const oldest = cache.keys().next().value as string;
    const entry = cache.get(oldest);
    cache.delete(oldest);
    if (entry) totalBytes -= entry.bytes;
  }
}

function store(key: string, text: string): void {
  const existing = cache.get(key);
  if (existing) {
    cache.delete(key);
    totalBytes -= existing.bytes;
  }
  const bytes = approximateBytes(text);
  cache.set(key, { text, bytes });
  totalBytes += bytes;
  evictIfNeeded();
}

/** 시험 전용: 캐시를 비우고 계정을 되돌린다. */
export function resetBodyCacheForTests(): { entries: number; bytes: number } {
  const stats = { entries: cache.size, bytes: totalBytes };
  cache.clear();
  totalBytes = 0;
  return stats;
}

/** 시험 전용: 현재 계정 상태. */
export function bodyCacheStats(): { entries: number; bytes: number } {
  return { entries: cache.size, bytes: totalBytes };
}

/**
 * artifact 본문 전문을 읽는다(페이지네이션 합침). 캐시 히트면 read를
 * 생략한다. 읽기 실패는 호출부가 영역 오류로 표시할 수 있게 그대로
 * 던진다 — 캐시에 오류를 저장하지 않는다(다음 시도에서 다시 읽는다).
 */
export async function readArtifactText(
  client: Pick<DaemonClient, "artifactRead">,
  artifactId: string,
  byteLength: number,
): Promise<string> {
  const key = artifactId;
  const hit = cache.get(key);
  if (hit) {
    // 접근 순서 갱신(LRU).
    cache.delete(key);
    cache.set(key, hit);
    return hit.text;
  }
  let offset = 0;
  let text = "";
  const decoder = new TextDecoder();
  for (;;) {
    const page = await client.artifactRead({ artifact_id: artifactId, offset: String(offset), max_bytes: 4096 });
    const nextOffset = Number(page.next_offset);
    if (!Number.isSafeInteger(nextOffset) || nextOffset < offset || (!page.complete && nextOffset === offset)) {
      throw new Error("Artifact cursor did not advance");
    }
    text += decoder.decode(decodeBase64(page.data_b64), { stream: true });
    offset = nextOffset;
    if (page.complete || (byteLength > 0 && offset >= byteLength)) break;
    if (page.data_b64.length === 0 && page.complete === false && Number(page.next_offset) === 0) break;
  }
  text += decoder.decode();
  store(key, text);
  return text;
}

function decodeBase64(b64: string): Uint8Array {
  const binary = atob(b64);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i++) bytes[i] = binary.charCodeAt(i);
  return bytes;
}
