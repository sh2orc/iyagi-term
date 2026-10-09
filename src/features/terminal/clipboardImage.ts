/**
 * 클립보드 이미지 → 임시 파일 경로 붙여넣기.
 *
 * 터미널은 그림을 받을 수 없지만 여기서 도는 것들(Claude Code·Codex·
 * opencode)은 전부 "경로"는 읽는다. 그래서 클립보드에 그림만 있으면 앱이
 * 임시 파일로 떨구고 그 경로를 붙여넣는다 — 스크린샷을 찍어 바로 Ctrl+Shift+V
 * 하면 에이전트가 읽을 수 있는 상태가 된다.
 *
 * 글과 그림이 함께 있으면(브라우저에서 복사할 때 흔하다) 글이 이긴다.
 * 터미널에 붙여넣기는 원래 글을 넣는 동작이고, 그림은 글이 없을 때의
 * 대안이어야 놀랍지 않다.
 *
 * 상한은 Rust 쪽 `MAX_IMAGE_BYTES`(bridge/paste_image.rs)와 같은 값이다.
 */

/** 임시 파일로 떨굴 수 있는 그림 형식 — 확장자는 Rust의 허용 목록과 같다. */
const IMAGE_EXTENSIONS: ReadonlyMap<string, string> = new Map([
  ["image/png", "png"],
  ["image/jpeg", "jpg"],
  ["image/gif", "gif"],
  ["image/webp", "webp"],
  ["image/bmp", "bmp"],
]);

/** 클립보드가 여러 형식을 함께 들고 있을 때의 선호 순서(무손실·널리 읽힘 순). */
const PREFERRED_TYPES: readonly string[] = ["image/png", "image/webp", "image/jpeg", "image/gif", "image/bmp"];

/** 32 MiB — 붙여넣기 한 번이 임시 폴더를 채우지 않게 하는 선. */
export const PASTE_IMAGE_MAX_BYTES = 32 * 1024 * 1024;

/**
 * 붙여넣을 그림 하나 — Blob·File이 그대로 들어맞는 최소 모양이다. 시험이
 * 진짜 Blob 없이도 controller를 돌릴 수 있게 구조로만 요구한다.
 */
export interface PasteImageSource {
  readonly type: string;
  arrayBuffer(): Promise<ArrayBuffer>;
}

/** MIME → 확장자(모르는 형식이면 null). 매개변수(`image/png;foo`)는 떼어 낸다. */
export function imageExtension(mime: string): string | null {
  const base = mime.split(";", 1)[0]?.trim().toLowerCase() ?? "";
  return IMAGE_EXTENSIONS.get(base) ?? null;
}

/** 이 형식 목록에서 쓸 만한 그림 하나(없으면 null). */
export function pickImageType(types: readonly string[]): string | null {
  const available = new Set(types.map((type) => type.split(";", 1)[0]?.trim().toLowerCase() ?? ""));
  return PREFERRED_TYPES.find((type) => available.has(type)) ?? null;
}

/** 붙여넣기 이벤트가 실어 온 글(공백뿐이어도 글은 글이다 — 그림보다 우선). */
export function pastedText(data: DataTransfer | null | undefined): string {
  return data?.getData("text/plain") ?? "";
}

/**
 * 붙여넣기·드롭 이벤트에서 그림 파일 하나. `items`가 없는 엔진을 위해
 * `files`로도 한 번 더 찾는다.
 */
export function imageFileOf(data: DataTransfer | null | undefined): File | null {
  if (!data) return null;
  for (const item of Array.from(data.items ?? [])) {
    if (item.kind !== "file" || imageExtension(item.type) === null) continue;
    const file = item.getAsFile();
    if (file) return file;
  }
  for (const file of Array.from(data.files ?? [])) {
    if (imageExtension(file.type) !== null) return file;
  }
  return null;
}

/**
 * 바이트 → base64. `btoa`는 인자 하나씩만 받으므로 32 KiB씩 끊어 넘긴다
 * (한 번에 펼치면 큰 그림에서 호출 스택이 넘친다).
 */
export function bytesToBase64(bytes: Uint8Array): string {
  const CHUNK = 0x8000;
  let binary = "";
  for (let offset = 0; offset < bytes.length; offset += CHUNK) {
    binary += String.fromCharCode(...bytes.subarray(offset, offset + CHUNK));
  }
  return btoa(binary);
}
