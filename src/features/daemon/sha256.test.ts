/**
 * Pure-JS SHA-256 vectors plus differential checks against node:crypto —
 * the mock daemon's artifact integrity path must agree with the daemon's
 * Rust sha2 on the other side of the wire.
 */

import { createHash } from "node:crypto";
import { describe, expect, it, vi } from "vitest";
import { sha256Hex, sha256HexAuto } from "./sha256";

function nodeSha256(bytes: Uint8Array): string {
  return createHash("sha256").update(bytes).digest("hex");
}

function utf8(text: string): Uint8Array {
  return new TextEncoder().encode(text);
}

/** Known NIST/FIPS vectors. */
const VECTORS: Array<[string, string]> = [
  ["", "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"],
  ["abc", "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"],
  [
    "abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq",
    "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1",
  ],
  // Exactly one block: exercises the boundary where the length suffix moves
  // the padding into an extra block.
  ["a".repeat(56), nodeSha256(utf8("a".repeat(56)))],
];

describe("sha256Hex (pure fallback)", () => {
  it.each(VECTORS)("matches the known digest of %j", (input, expected) => {
    expect(sha256Hex(utf8(input))).toBe(expected);
  });

  it("hashes multibyte UTF-8 identically to node:crypto", () => {
    const bytes = utf8("목표: 한글 payloads travel through artifact uploads");
    expect(sha256Hex(bytes)).toBe(nodeSha256(bytes));
  });

  it("stays correct across the 55/56/64-byte padding boundaries", () => {
    for (const len of [54, 55, 56, 57, 63, 64, 65, 119, 120, 128, 1000]) {
      const bytes = new Uint8Array(len).map((_, i) => (i * 7) & 0xff);
      expect(sha256Hex(bytes)).toBe(nodeSha256(bytes));
    }
  });
});

describe("sha256HexAuto (Web Crypto preferred)", () => {
  it("matches the pure implementation on node's webcrypto path", async () => {
    const bytes = new Uint8Array(5000).map((_, i) => (i * 31) & 0xff);
    expect(await sha256HexAuto(bytes)).toBe(sha256Hex(bytes));
  });

  it("falls back to the pure implementation without subtle", async () => {
    vi.stubGlobal("crypto", { subtle: undefined });
    try {
      const bytes = utf8("no subtle here");
      expect(await sha256HexAuto(bytes)).toBe(nodeSha256(bytes));
    } finally {
      vi.unstubAllGlobals();
    }
  });
});
