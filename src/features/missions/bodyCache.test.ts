import { beforeEach, expect, it } from "vitest";
import type { DaemonClient } from "../daemon/client";
import { readArtifactText, resetBodyCacheForTests } from "./bodyCache";
beforeEach(() => { resetBodyCacheForTests(); });
it("decodes a multibyte character split across artifact pages", async () => {
  const bytes = new TextEncoder().encode("a\uAC00b");
  const client = { artifactRead: async ({ offset }: { offset: string }) => {
    const start = Number(offset), end = Math.min(start + 2, bytes.length);
    return { data_b64: btoa(String.fromCharCode(...bytes.slice(start, end))), next_offset: String(end), complete: end === bytes.length };
  } } satisfies Pick<DaemonClient, "artifactRead">;
  expect(await readArtifactText(client, "unicode", bytes.length)).toBe("a\uAC00b");
});
it("rejects a stalled artifact cursor instead of looping forever", async () => {
  const client = { artifactRead: async () => ({ data_b64: "", next_offset: "0", complete: false }) } satisfies Pick<DaemonClient, "artifactRead">;
  await expect(readArtifactText(client, "stalled", 100)).rejects.toThrow("cursor");
});
