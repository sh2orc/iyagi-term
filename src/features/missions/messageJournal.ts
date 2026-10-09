import type { MissionMessageParams } from "../../generated/MissionMessageParams";
import type { ArtifactRef } from "../../generated/ArtifactRef";
import type { DaemonClient } from "../daemon/client";
import { MISSION_TEXT_MAX_BYTES } from "./artifactUpload";

export const MESSAGE_JOURNAL_DB = "iyagi.mission-message-recovery.v1";
const TABLE = "pending";
export const messageScope = (missionId: string, taskId: string | null, sourceId: string | null = null) => JSON.stringify([missionId, taskId, sourceId]);
const scopeOf = (p: MissionMessageParams) => messageScope(p.mission_id, p.target_task_id, p.supersedes_message_id ?? null);

export class PendingMessageConflict extends Error {
  constructor(readonly pending: MissionMessageParams) { super("Another send still requires confirmation"); }
}

export interface MessageJournal {
  load(scope: string): Promise<MissionMessageParams | null>;
  save(params: MissionMessageParams): Promise<void>;
  forget(params: MissionMessageParams): Promise<void>;
}

function object(value: unknown): value is Record<string, unknown> {
  return !!value && typeof value === "object" && !Array.isArray(value);
}
function keys(value: Record<string, unknown>, allowed: string[]): boolean {
  return Object.keys(value).every(key => allowed.includes(key));
}
function id(value: unknown): value is string {
  return typeof value === "string" && /^[a-zA-Z0-9-]{1,128}$/.test(value);
}
function integer(value: unknown, maximum = 9223372036854775807n): value is string {
  return typeof value === "string" && /^(0|[1-9][0-9]{0,18})$/.test(value) && BigInt(value) <= maximum;
}

/** Restore only this scope's bounded wire fields. Never trust stored extra keys. */
export function decodePendingMessage(raw: unknown, scope: string): MissionMessageParams {
  if (!object(raw) || !keys(raw, ["version", "params"]) || raw.version !== 1 || !object(raw.params)) throw new Error("Invalid message recovery record");
  const p = raw.params, ref = p.body_ref;
  if (!keys(p, ["request_id", "mission_id", "expected_revision", "target_task_id", "body_ref", "supersedes_message_id"])
    || !id(p.request_id) || !id(p.mission_id) || !(p.target_task_id === null || id(p.target_task_id))
    || !integer(p.expected_revision) || !object(ref) || !id(ref.id)
    || !keys(ref, ["id", "bytes", "sha256", "media_type"])
    || !integer(ref.bytes, BigInt(MISSION_TEXT_MAX_BYTES)) || typeof ref.sha256 !== "string" || !/^[a-f0-9]{64}$/.test(ref.sha256)
    || typeof ref.media_type !== "string" || !/^text\/plain(?:; charset=utf-8)?$/.test(ref.media_type)
    || !(p.supersedes_message_id === undefined || p.supersedes_message_id === null || id(p.supersedes_message_id))) {
    throw new Error("Invalid message recovery fields");
  }
  const params: MissionMessageParams = { request_id: p.request_id, mission_id: p.mission_id,
    expected_revision: p.expected_revision, target_task_id: p.target_task_id,
    body_ref: { id: ref.id, bytes: ref.bytes, sha256: ref.sha256, media_type: ref.media_type },
    ...(p.supersedes_message_id !== undefined ? { supersedes_message_id: p.supersedes_message_id } : {}) };
  if (scopeOf(params) !== scope) throw new Error("Message recovery recipient does not match");
  return params;
}

const encode = (params: MissionMessageParams) => ({ version: 1, params: decodePendingMessage({ version: 1, params }, scopeOf(params)) });
const same = (a: MissionMessageParams, b: MissionMessageParams) => JSON.stringify(encode(a)) === JSON.stringify(encode(b));

/** IDB readwrite transactions serialize the recipient slot across app windows.
 * Resolve only on transaction completion, before allowing any mission mutation.
 * No message text, credentials, or daemon snapshots are copied to this database.
 */
export function createMessageJournal(factory: IDBFactory = indexedDB): MessageJournal {
  let opening: Promise<IDBDatabase> | undefined;
  const open = () => opening ??= new Promise<IDBDatabase>((resolve, reject) => {
    let abandoned = false;
    const request = factory.open(MESSAGE_JOURNAL_DB, 1);
    request.onupgradeneeded = () => { request.result.createObjectStore(TABLE); };
    request.onerror = () => { abandoned = true; opening = undefined; reject(new Error("Message recovery storage could not be opened")); };
    request.onblocked = () => { abandoned = true; opening = undefined; reject(new Error("Message recovery storage is blocked by another app window")); };
    request.onsuccess = () => {
      const db = request.result;
      if (abandoned) { db.close(); return; }
      db.onversionchange = () => { db.close(); opening = undefined; };
      resolve(db);
    };
  });
  async function transact(scope: string, action: "load" | "save" | "forget", params?: MissionMessageParams) {
    const db = await open();
    return new Promise<MissionMessageParams | null>((resolve, reject) => {
      const tx = db.transaction(TABLE, action === "load" ? "readonly" : "readwrite", { durability: "strict" });
      const store = tx.objectStore(TABLE);
      let value: MissionMessageParams | null = null;
      let failure: unknown;
      tx.oncomplete = () => resolve(value);
      tx.onabort = () => reject(failure ?? new Error("Message recovery storage transaction failed"));
      tx.onerror = () => { failure ??= new Error("Message recovery storage transaction failed"); };
      const read = store.get(scope);
      read.onsuccess = () => {
        try {
          value = read.result === undefined ? null : decodePendingMessage(read.result, scope);
          if (action === "load") return;
          if (value && !same(value, params!)) throw new PendingMessageConflict(value);
          if (action === "save") store.put(encode(params!), scope);
          else store.delete(scope);
        } catch (error) { failure = error; tx.abort(); }
      };
    });
  }
  return {
    load: scope => transact(scope, "load"),
    save: async params => { await transact(scopeOf(params), "save", params); },
    forget: async params => { await transact(scopeOf(params), "forget", params); },
  };
}

let journal: MessageJournal | undefined;
export function getMessageJournal(): MessageJournal {
  return journal ??= createMessageJournal();
}
/** Explicit test seam; production never falls back to volatile storage. */
export function setMessageJournalForTests(value: MessageJournal): void { journal = value; }

export async function readPendingMessageBody(client: Pick<DaemonClient, "artifactRead">, ref: ArtifactRef): Promise<string> {
  const length = Number(ref.bytes);
  if (!Number.isSafeInteger(length) || length < 0 || length > MISSION_TEXT_MAX_BYTES) throw new Error("Invalid recovery body length");
  const bytes = new Uint8Array(length);
  let offset = 0;
  for (;;) {
    const page = await client.artifactRead({ artifact_id: ref.id, offset: String(offset), max_bytes: Math.min(4096, Math.max(1, length - offset)) });
    if (page.data_b64.length > 5464 || !integer(page.next_offset, BigInt(length))) throw new Error("Invalid recovery body page");
    const chunk = Uint8Array.from(atob(page.data_b64), c => c.charCodeAt(0));
    const next = Number(page.next_offset);
    if (next !== offset + chunk.length || next > length || (next === offset && !page.complete)) throw new Error("Invalid recovery body cursor");
    bytes.set(chunk, offset); offset = next;
    if (page.complete) { if (offset !== length) throw new Error("Incomplete recovery body"); break; }
    if (offset === length) throw new Error("Recovery body exceeds its recorded length");
  }
  const digest = await crypto.subtle.digest("SHA-256", bytes);
  const hash = [...new Uint8Array(digest)].map(b => b.toString(16).padStart(2, "0")).join("");
  if (hash !== ref.sha256) throw new Error("Recovery body hash does not match");
  return new TextDecoder("utf-8", { fatal: true }).decode(bytes);
}
