/**
 * Base64 helpers that work in both the browser bundle and node (vitest).
 * Never used for anything security-relevant — transport encoding only.
 */

export function bytesToBase64(bytes: Uint8Array): string {
  let binary = "";
  for (let i = 0; i < bytes.length; i++) binary += String.fromCharCode(bytes[i]);
  if (typeof btoa === "function") return btoa(binary);
  const Buf = (globalThis as { Buffer?: { from(data: Uint8Array): { toString(enc: string): string } } }).Buffer;
  if (Buf) return Buf.from(bytes).toString("base64");
  throw new Error("no base64 encoder available");
}

export function base64ToBytes(text: string): Uint8Array {
  if (typeof atob === "function") {
    const binary = atob(text);
    const out = new Uint8Array(binary.length);
    for (let i = 0; i < binary.length; i++) out[i] = binary.charCodeAt(i);
    return out;
  }
  const Buf = (globalThis as { Buffer?: { from(data: string, enc: string): Uint8Array } }).Buffer;
  if (Buf) return new Uint8Array(Buf.from(text, "base64"));
  throw new Error("no base64 decoder available");
}
