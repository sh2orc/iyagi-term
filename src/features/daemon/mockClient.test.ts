/**
 * MockDaemonClient O1 mission store tests (ticket O04): the fake must
 * reproduce the contract behaviors the mission UI will lean on — CAS
 * mutations with structured REVISION_CONFLICT details, request_id
 * idempotency, snapshot materialization/paging, the mission.changed hint
 * stream, and artifact chunk-upload integrity.
 */

import { createHash } from "node:crypto";
import { describe, expect, it } from "vitest";
import type { Binding } from "../../generated/Binding";
import type { Candidate } from "../../generated/Candidate";
import type { Decision } from "../../generated/Decision";
import type { LaunchRequest } from "../../generated/LaunchRequest";
import type { MissionCreateParams } from "../../generated/MissionCreateParams";
import type { Policy } from "../../generated/Policy";
import type { Run } from "../../generated/Run";
import type { Task } from "../../generated/Task";
import type { TeamTemplate } from "../../generated/TeamTemplate";
import type { VerificationCommand } from "../../generated/VerificationCommand";
import type { Workspace } from "../../generated/Workspace";
import { RpcClientError } from "./client";
import { MockDaemonClient } from "./mockClient";
import { base64ToBytes, bytesToBase64 } from "./base64";

function nodeSha256(bytes: Uint8Array): string {
  return createHash("sha256").update(bytes).digest("hex");
}

function utf8(text: string): Uint8Array {
  return new TextEncoder().encode(text);
}

/** Manual scheduler: mission.changed hints stay queued until drain(). */
function newMock() {
  const queue: Array<() => void> = [];
  let uuidSeq = 0;
  const client = new MockDaemonClient({
    resourceIntervalMs: 0,
    schedule: (fn) => queue.push(fn),
    uuid: () => `uuid-${++uuidSeq}`,
  });
  const drain = (): void => {
    for (let guard = 0; queue.length > 0 && guard < 1000; guard++) {
      (queue.shift() as () => void)();
    }
  };
  return { client, drain };
}

function fakePolicy(): Policy {
  return {
    max_parallel_runs: 2,
    max_attempts_per_task: 3,
    max_repair_cycles: 2,
    max_automatic_starts: 1,
    active_time_limit_ms: "3600000",
    run_time_limit_ms: "1800000",
    max_cost_usd_micros: null,
    unknown_cost: "allow_with_notice",
    allow_network: false,
    allow_automatic_plan_apply: false,
    allow_recovery_of_unsent: false,
    allowed_binding_ids: [],
    allowed_roles: ["lead", "builder"],
    allowed_verification_ids: [],
    require_independent_review: true,
    require_enforced_verification: true,
  };
}

function createParams(overrides: Partial<MissionCreateParams> = {}): MissionCreateParams {
  return {
    request_id: "req-create",
    title: "모아이 터미널 출시",
    repository_path: "D:/repo/iyagi",
    expected_base_oid: "b".repeat(40),
    goal_ref: { id: "goal-1", sha256: "a".repeat(64), bytes: "128", media_type: "text/markdown" },
    requirements: [],
    policy: fakePolicy(),
    role_bindings: [{ role: "lead", primary_binding_id: "binding-1", fallback_binding_ids: [] }],
    ...overrides,
  };
}

function fakeBinding(id = "binding-1"): Binding {
  const yes = { supported: true, reason_code: null };
  const no = { supported: false, reason_code: null };
  return {
    id,
    revision: "0",
    label: "codex",
    runtime: "codex",
    program: "C:/bin/codex.exe",
    runtime_version: null,
    provider_id: "openai",
    model_id: "gpt-5-codex",
    effort: null,
    auth_route: "subscription",
    credential_ref: null,
    endpoint_ref: null,
    capabilities: {
      structured_result: yes,
      events: yes,
      cancel: yes,
      resume: { supported: false, reason_code: "unsupported" },
      steer: no,
      approval_reply: no,
      read_only: yes,
      scoped_write: yes,
      model_listing: yes,
      usage: yes,
      native_terminal_attach: no,
    },
    checked_at: null,
    enabled: true,
    estimated_run_cost_usd_micros: null,
    experimental_version: null,
    local_evidence: null,
    resource_policy: {
      reservation_bytes: "268435456",
      cpu_slots: 2,
      enforcement: "observe",
      memory_max_bytes: null,
      cpu_max_cores: null,
      pids_max: null,
    },
  };
}

/** Open product decision the tests seed into an existing mission. */
function fakeDecision(id: string, missionId: string): Decision {
  return {
    id,
    mission_id: missionId,
    requesting_run_id: null,
    kind: "product",
    state: "open",
    question_ref: { id: "q-1", sha256: "c".repeat(64), bytes: "64", media_type: "text/plain" },
    options: [
      { id: "ship", label: "Ship it" },
      { id: "hold", label: "Hold" },
    ],
    affected_task_ids: [],
    blocking: true,
    plan_revision: 0,
    candidate_id: null,
    answer_ref: null,
    selected_option_id: null,
    answer_message_id: null,
    created_at: "2026-09-13T00:00:00Z",
    answered_at: null,
  };
}

describe("MockDaemonClient mission create/snapshot/events round trip", () => {
  it("creates a draft mission, pages its snapshot, and lists it", async () => {
    const { client } = newMock();
    const created = await client.missionCreate(createParams());
    expect(created).toMatchObject({ revision: "1", event_seq: "1" });
    expect(created.entity_ids).toEqual([created.mission_id]);

    const page = await client.missionSnapshot({
      mission_id: created.mission_id,
      snapshot_id: null,
      cursor: null,
    });
    expect(page.entities).toHaveLength(1);
    expect(page.entities[0]).toEqual({
      kind: "mission",
      value: expect.objectContaining({
        id: created.mission_id,
        state: "draft",
        phase: "planning",
        revision: "1",
        title: "모아이 터미널 출시",
        repository_path: "D:/repo/iyagi",
        goal_ref: createParams().goal_ref,
      }),
    });
    expect(page.at_seq).toBe("1");
    expect(page.revision).toBe("1");
    expect(page.next_cursor).toBeNull();
    expect(typeof page.snapshot_id).toBe("string");

    const events = await client.missionEvents({
      mission_id: created.mission_id,
      after_seq: "0",
      limit: 50,
    });
    expect(events.events.map((e) => e.type)).toEqual(["created"]);
    expect(events.events[0].changes).toEqual([
      { entity_kind: "mission", entity_id: created.mission_id, operation: "upsert" },
    ]);
    expect(events.high_watermark).toBe("1");
    expect(events.next_after_seq).toBe("1");

    const list = await client.missionList({ cursor: null, limit: 50, archived: false });
    expect(list.items.map((m) => m.id)).toEqual([created.mission_id]);
    expect(list.next_cursor).toBeNull();
  });

  it("emits mission.changed hints batched through the injected scheduler", async () => {
    const { client, drain } = newMock();
    const changed: Array<{ mission_id: string; latest_seq: string }> = [];
    client.events.subscribe((event) => {
      if (event.kind === "mission.changed") changed.push(event.payload);
    });

    const created = await client.missionCreate(createParams());
    expect(changed).toEqual([]); // caller continuation first, then the hint
    drain();
    expect(changed).toEqual([{ mission_id: created.mission_id, latest_seq: "1" }]);
  });

  it("shares one repository_id between missions on the same path", async () => {
    const { client } = newMock();
    const a = await client.missionCreate(createParams({ request_id: "req-a" }));
    const b = await client.missionCreate(
      createParams({ request_id: "req-b", repository_path: "D:/repo/other" }),
    );
    const missions = client.missions;
    const repoA = missions.find((m) => m.id === a.mission_id)?.repository_id;
    const repoB = missions.find((m) => m.id === b.mission_id)?.repository_id;
    expect(repoA).not.toBe(repoB);
    const c = await client.missionCreate(createParams({ request_id: "req-c" }));
    expect(client.missions.find((m) => m.id === c.mission_id)?.repository_id).toBe(repoA);
  });
});

describe("MockDaemonClient mission mutations", () => {
  async function seedMission() {
    const { client, drain } = newMock();
    const created = await client.missionCreate(createParams());
    return { client, drain, missionId: created.mission_id };
  }

  it("bumps revision and event seq together per transaction", async () => {
    const { client, missionId } = await seedMission();
    const start = await client.missionControl({
      request_id: "req-start",
      mission_id: missionId,
      expected_revision: "1",
      action: "start",
    });
    expect(start).toMatchObject({ mission_id: missionId, revision: "2", event_seq: "2" });

    const snapshot = await client.missionSnapshot({ mission_id: missionId, snapshot_id: null, cursor: null });
    expect(snapshot.entities[0]).toMatchObject({ kind: "mission", value: { state: "running" } });

    const events = await client.missionEvents({ mission_id: missionId, after_seq: "0", limit: 10 });
    expect(events.events.map((e) => e.type)).toEqual(["created", "changed"]);
    expect(events.high_watermark).toBe("2");

    const tail = await client.missionEvents({ mission_id: missionId, after_seq: "1", limit: 10 });
    expect(tail.events).toHaveLength(1);
    expect(tail.next_after_seq).toBe("2");
  });

  it("rejects stale expected_revision with current_revision in details", async () => {
    const { client, missionId } = await seedMission();
    await client.missionControl({
      request_id: "req-start",
      mission_id: missionId,
      expected_revision: "1",
      action: "start",
    });

    const error = await client
      .missionControl({ request_id: "req-stale", mission_id: missionId, expected_revision: "1", action: "pause" })
      .catch((err: unknown) => err);
    expect(error).toBeInstanceOf(RpcClientError);
    expect(error).toMatchObject({
      code: "REVISION_CONFLICT",
      retryable: false,
      details: { current_revision: "2" },
    });

    // 거절은 side effect를 남기지 않는다(§2).
    const events = await client.missionEvents({ mission_id: missionId, after_seq: "0", limit: 10 });
    expect(events.high_watermark).toBe("2");
  });

  it("replays the stored first response for a duplicate request_id (no new event)", async () => {
    const { client, drain, missionId } = await seedMission();
    const changed: string[] = [];
    client.events.subscribe((event) => {
      if (event.kind === "mission.changed") changed.push(event.payload.latest_seq);
    });

    const first = await client.missionControl({
      request_id: "req-start",
      mission_id: missionId,
      expected_revision: "1",
      action: "start",
    });
    // 같은 request_id 재전송 — revision 재검사 없이 저장된 최초 응답(§2).
    const replay = await client.missionControl({
      request_id: "req-start",
      mission_id: missionId,
      expected_revision: "1",
      action: "start",
    });
    expect(replay).toEqual(first);
    expect(replay).not.toBe(first);
    drain();
    expect(changed).toEqual(["1", "2"]); // create + start 1건 — 재전송은 hint 없음

    // 같은 request_id에 다른 payload는 REQUEST_CONFLICT.
    await expect(
      client.missionControl({ request_id: "req-start", mission_id: missionId, expected_revision: "1", action: "pause" }),
    ).rejects.toMatchObject({ code: "REQUEST_CONFLICT" });

    // 타임아웃 복구 경로: request.get이 최초 응답을 돌려준다.
    const got = await client.missionRequestGet({ request_id: "req-start" });
    expect(got).toEqual({ state: "committed", result: first });
    expect(await client.missionRequestGet({ request_id: "req-unknown" })).toEqual({
      state: "not_found",
      result: null,
    });
  });

  it("answers a seeded open decision and reports STALE_DECISION afterwards", async () => {
    const { client, missionId } = await seedMission();
    const decision = fakeDecision("decision-1", missionId);
    client.seedMissionEntity(missionId, { kind: "decision", value: decision });

    const answered = await client.missionDecisionAnswer({
      request_id: "req-answer",
      mission_id: missionId,
      expected_revision: "1",
      decision_id: "decision-1",
      option_id: "ship",
      answer_ref: null,
    });
    expect(answered.revision).toBe("2");

    const snapshot = await client.missionSnapshot({ mission_id: missionId, snapshot_id: null, cursor: null });
    expect(snapshot.entities).toHaveLength(2);
    expect(snapshot.entities[1]).toMatchObject({
      kind: "decision",
      value: { state: "answered", selected_option_id: "ship" },
    });

    const stale = await client
      .missionDecisionAnswer({
        request_id: "req-answer-2",
        mission_id: missionId,
        expected_revision: "2",
        decision_id: "decision-1",
        option_id: "hold",
        answer_ref: null,
      })
      .catch((err: unknown) => err);
    expect(stale).toBeInstanceOf(RpcClientError);
    expect(stale).toMatchObject({ code: "STALE_DECISION" });
    // 지금 열려 있는 질문이 없으므로 decision_id는 내려가지 않는다(§7).
    expect((stale as RpcClientError).details?.decision_id).toBeUndefined();
  });

  it("archives only terminal missions (INVALID_STATE otherwise)", async () => {
    const { client, missionId } = await seedMission();
    await expect(
      client.missionControl({ request_id: "req-archive", mission_id: missionId, expected_revision: "1", action: "archive" }),
    ).rejects.toMatchObject({ code: "INVALID_STATE" });

    await client.missionControl({ request_id: "req-cancel", mission_id: missionId, expected_revision: "1", action: "cancel" });
    await client.missionControl({ request_id: "req-archive", mission_id: missionId, expected_revision: "2", action: "archive" });

    const active = await client.missionList({ cursor: null, limit: 50, archived: false });
    const archived = await client.missionList({ cursor: null, limit: 50, archived: true });
    expect(active.items).toHaveLength(0);
    expect(archived.items.map((m) => m.id)).toEqual([missionId]);
  });
});

describe("MockDaemonClient snapshot paging and immutability", () => {
  it("caps pages at 50 entities and freezes the view at materialization", async () => {
    const { client } = newMock();
    const created = await client.missionCreate(createParams());
    const missionId = created.mission_id;
    const body = { id: "body-1", sha256: "d".repeat(64), bytes: "32", media_type: "text/plain" };

    // 1 mission + 55 messages = 56 entities → 2 pages.
    for (let i = 1; i <= 55; i++) {
      await client.missionMessage({
        request_id: `req-msg-${i}`,
        mission_id: missionId,
        expected_revision: String(i),
        target_task_id: null,
        body_ref: body,
      });
    }
    expect(client.missions[0].revision).toBe("56");

    const page1 = await client.missionSnapshot({ mission_id: missionId, snapshot_id: null, cursor: null });
    expect(page1.entities).toHaveLength(50);
    expect(page1.next_cursor).toBe("50");
    expect(page1.at_seq).toBe("56");

    // materialize 이후 mutation — 이미 만들어진 snapshot은 불변(§4).
    await client.missionControl({ request_id: "req-start", mission_id: missionId, expected_revision: "56", action: "start" });

    const page2 = await client.missionSnapshot({
      mission_id: missionId,
      snapshot_id: page1.snapshot_id,
      cursor: page1.next_cursor,
    });
    expect(page2.entities).toHaveLength(6);
    expect(page2.next_cursor).toBeNull();
    expect(page2.at_seq).toBe("56"); // frozen at materialization, not 57
    expect(page2.snapshot_id).toBe(page1.snapshot_id);
    expect(page1.entities[0]).toMatchObject({ kind: "mission", value: { revision: "56", state: "draft" } });
  });

  it("rejects cross-mission snapshot ids with INVALID_ARGUMENT", async () => {
    const { client } = newMock();
    const a = await client.missionCreate(createParams({ request_id: "req-a" }));
    const b = await client.missionCreate(createParams({ request_id: "req-b" }));
    const page = await client.missionSnapshot({ mission_id: a.mission_id, snapshot_id: null, cursor: null });
    await expect(
      client.missionSnapshot({ mission_id: b.mission_id, snapshot_id: page.snapshot_id, cursor: null }),
    ).rejects.toMatchObject({ code: "INVALID_ARGUMENT" });
  });

  it("reports unknown missions as NOT_FOUND", async () => {
    const { client } = newMock();
    await expect(
      client.missionSnapshot({ mission_id: "nope", snapshot_id: null, cursor: null }),
    ).rejects.toMatchObject({ code: "NOT_FOUND" });
  });
});

describe("MockDaemonClient artifact uploads", () => {
  it("round-trips a chunked upload: begin → write → commit → read", async () => {
    const { client } = newMock();
    const payload = utf8("iyagi artifact payload — ".repeat(450)); // 9,450 bytes
    const sha = nodeSha256(payload);

    const begin = await client.artifactBegin({
      request_id: "up-1",
      mission_id: null,
      media_type: "text/plain",
      bytes: String(payload.length),
      sha256: sha,
    });
    expect(begin.chunk_bytes).toBe(4096);

    for (let offset = 0; offset < payload.length; offset += 4096) {
      const end = Math.min(offset + 4096, payload.length);
      const res = await client.artifactWrite({
        upload_id: begin.upload_id,
        offset: String(offset),
        data_b64: bytesToBase64(payload.subarray(offset, end)),
      });
      expect(res.next_offset).toBe(String(end));
    }

    // 동일 offset·동일 bytes 재전송은 같은 next_offset(§3).
    const dup = await client.artifactWrite({
      upload_id: begin.upload_id,
      offset: "0",
      data_b64: bytesToBase64(payload.subarray(0, 4096)),
    });
    expect(dup.next_offset).toBe("4096");

    const ref = await client.artifactCommit({ upload_id: begin.upload_id });
    expect(ref).toMatchObject({ sha256: sha, bytes: String(payload.length), media_type: "text/plain" });
    // commit 재호출은 같은 ref.
    expect(await client.artifactCommit({ upload_id: begin.upload_id })).toEqual(ref);

    // read: 4 KiB 단위로 next_offset을 따라 끝까지 읽으면 원본과 같다.
    const chunks: Uint8Array[] = [];
    let offset = 0;
    let complete = false;
    while (!complete) {
      const res = await client.artifactRead({ artifact_id: ref.id, offset: String(offset), max_bytes: 4096 });
      chunks.push(base64ToBytes(res.data_b64));
      offset = Number(res.next_offset);
      complete = res.complete;
      expect(complete).toBe(offset >= payload.length);
    }
    const whole = new Uint8Array(chunks.reduce((n, c) => n + c.length, 0));
    let at = 0;
    for (const chunk of chunks) {
      whole.set(chunk, at);
      at += chunk.length;
    }
    expect(whole).toEqual(payload);
  });

  it("rejects conflicting bytes at the same offset and skipped offsets", async () => {
    const { client } = newMock();
    const payload = utf8("0123456789abcdef");
    const begin = await client.artifactBegin({
      request_id: "up-1",
      mission_id: null,
      media_type: "text/plain",
      bytes: String(payload.length),
      sha256: nodeSha256(payload),
    });
    await client.artifactWrite({ upload_id: begin.upload_id, offset: "0", data_b64: bytesToBase64(payload.subarray(0, 8)) });

    // 같은 offset, 다른 bytes.
    await expect(
      client.artifactWrite({ upload_id: begin.upload_id, offset: "0", data_b64: bytesToBase64(utf8("XXXXXXXX")) }),
    ).rejects.toMatchObject({ code: "REQUEST_CONFLICT" });
    // 건너뛴 offset.
    await expect(
      client.artifactWrite({ upload_id: begin.upload_id, offset: "12", data_b64: bytesToBase64(payload.subarray(8)) }),
    ).rejects.toMatchObject({ code: "INVALID_ARGUMENT" });
    // 알 수 없는 upload.
    await expect(
      client.artifactWrite({ upload_id: "ghost", offset: "0", data_b64: "aGk=" }),
    ).rejects.toMatchObject({ code: "NOT_FOUND" });
  });

  it("fails commit with INTEGRITY_FAILED on hash/length mismatch", async () => {
    const { client } = newMock();
    const payload = utf8("hello");

    const wrongHash = await client.artifactBegin({
      request_id: "up-hash",
      mission_id: null,
      media_type: "text/plain",
      bytes: "5",
      sha256: nodeSha256(utf8("different")),
    });
    await client.artifactWrite({ upload_id: wrongHash.upload_id, offset: "0", data_b64: bytesToBase64(payload) });
    await expect(client.artifactCommit({ upload_id: wrongHash.upload_id })).rejects.toMatchObject({
      code: "INTEGRITY_FAILED",
    });

    const wrongLength = await client.artifactBegin({
      request_id: "up-len",
      mission_id: null,
      media_type: "text/plain",
      bytes: "4", // 실제는 5 bytes
      sha256: nodeSha256(payload),
    });
    await client.artifactWrite({ upload_id: wrongLength.upload_id, offset: "0", data_b64: bytesToBase64(payload) });
    await expect(client.artifactCommit({ upload_id: wrongLength.upload_id })).rejects.toMatchObject({
      code: "INTEGRITY_FAILED",
    });
  });

  it("replays begin for a duplicate request_id and requires a known mission", async () => {
    const { client } = newMock();
    const sha = nodeSha256(utf8("hi"));
    const first = await client.artifactBegin({
      request_id: "up-1",
      mission_id: null,
      media_type: "text/plain",
      bytes: "2",
      sha256: sha,
    });
    const replay = await client.artifactBegin({
      request_id: "up-1",
      mission_id: null,
      media_type: "text/plain",
      bytes: "2",
      sha256: sha,
    });
    expect(replay).toEqual(first);

    await expect(
      client.artifactBegin({
        request_id: "up-2",
        mission_id: "ghost-mission",
        media_type: "text/plain",
        bytes: "2",
        sha256: sha,
      }),
    ).rejects.toMatchObject({ code: "NOT_FOUND" });
  });
});

describe("MockDaemonClient runtime.detect", () => {
  it("returns a deterministic, read-only detection fixture in wire order", async () => {
    const { client } = newMock();
    const before = await client.bindingList();
    const first = await client.runtimeDetect();
    expect(first.runtimes.map((r) => r.runtime)).toEqual(["codex", "claude", "opencode"]);
    expect(first.runtimes[0]).toMatchObject({
      program: "/opt/homebrew/bin/codex",
      version: "0.154.0",
      installation: "verified",
      login: "found",
      suggested_provider_id: "openai",
      proven_model_id: "gpt-5.6-luna",
      verified_roles: ["lead", "builder", "reviewer", "integrator"],
    });
    expect(first.runtimes[1]).toMatchObject({ runtime: "claude", installation: "verified", verified_roles: [] });
    expect(first.runtimes[2]).toEqual({
      runtime: "opencode",
      program: "",
      version: null,
      installation: "not_found",
      login: "unknown",
      configured_model_id: null,
      suggested_provider_id: "zai-coding-plan",
      proven_model_id: null,
      models: [{ id: "glm-5.3", efforts: [] }],
      verified_roles: [],
      grade: "not_installed",
      experimental_roles: [],
    });
    expect(first.runtimes[0]).toMatchObject({ grade: "verified" });
    // 출시 증거는 없지만 값싼 자가 진단이 통과한 행: 동의 없이 네 역할을 쓸 수 있다(11 §7).
    expect(first.runtimes[1]).toMatchObject({
      grade: "verified_locally",
      experimental_roles: ["lead", "builder", "reviewer", "integrator"],
    });
    // Each call is a fresh value and nothing is saved as a binding.
    first.runtimes[0].verified_roles.length = 0;
    expect((await client.runtimeDetect()).runtimes[0].verified_roles).toHaveLength(4);
    expect(await client.bindingList()).toEqual(before);
  });

  it("probe verifies automated-run capabilities only for the detected Codex evidence combination", async () => {
    const { client } = newMock();
    const unchecked: { supported: boolean; reason_code: string | null } = { supported: false, reason_code: "not_checked" };
    const base: Binding = {
      ...fakeBinding("binding-evidence"),
      program: "/opt/homebrew/bin/codex",
      model_id: "gpt-5.6-luna",
      capabilities: Object.fromEntries(
        Object.keys(fakeBinding().capabilities).map((key) => [key, unchecked]),
      ) as Binding["capabilities"],
    };
    await client.bindingSave({ request_id: "e-1", expected_revision: "0", binding: base });
    const probe = await client.bindingProbe({ binding_id: base.id });
    expect(probe.installation).toBe("verified");
    expect(probe.binding.runtime_version).toBe("0.154.0");
    const verified = { supported: true, reason_code: null };
    expect(probe.binding.capabilities).toEqual({
      ...base.capabilities,
      structured_result: verified,
      events: verified,
      cancel: verified,
      read_only: verified,
      scoped_write: verified,
    });
    expect((await client.bindingList()).bindings[0]).toEqual(probe.binding);

    // Another model on the same installed CLI is not the shipped combination, but the
    // local self-check is model-independent (11 §6): the adapter's implemented
    // capabilities open as `local_probe`, the rest stay `adapter_not_implemented`.
    const otherModel: Binding = { ...base, id: "binding-model", model_id: "gpt-5.5" };
    await client.bindingSave({ request_id: "save-binding-model", expected_revision: "0", binding: otherModel });
    const locally = await client.bindingProbe({ binding_id: otherModel.id });
    expect(locally.binding.runtime_version).toBe("0.154.0");
    const probed = { supported: true, reason_code: "local_probe" };
    const unimplemented = { supported: false, reason_code: "adapter_not_implemented" };
    expect(locally.binding.capabilities).toEqual({
      structured_result: probed,
      events: probed,
      cancel: probed,
      resume: unimplemented,
      steer: probed,
      approval_reply: probed,
      read_only: probed,
      scoped_write: probed,
      model_listing: probed,
      usage: probed,
      native_terminal_attach: unimplemented,
    });
    expect(locally.binding.local_evidence?.probe?.protocol_ok).toBe(true);

    // A route the adapter cannot launch, or a disabled connection, keeps what was saved.
    const others: Binding[] = [
      { ...base, id: "binding-api", auth_route: "api_key" },
      { ...base, id: "binding-disabled", enabled: false },
    ];
    for (const binding of others) {
      await client.bindingSave({ request_id: `save-${binding.id}`, expected_revision: "0", binding });
      const other = await client.bindingProbe({ binding_id: binding.id });
      expect(other.binding.capabilities).toEqual(base.capabilities);
      expect(other.binding.runtime_version).toBeNull();
    }
  });
});

describe("MockDaemonClient binding/template/verification stores", () => {
  it("saves with CAS from revision 0 and bumps once per save", async () => {
    const { client } = newMock();
    const binding = fakeBinding();
    const saved = await client.bindingSave({ request_id: "b-1", expected_revision: "0", binding });
    expect(saved.binding.revision).toBe("1");

    await expect(
      client.bindingSave({ request_id: "b-2", expected_revision: "0", binding }),
    ).rejects.toMatchObject({ code: "REVISION_CONFLICT", details: { current_revision: "1" } });

    const updated = await client.bindingSave({
      request_id: "b-3",
      expected_revision: "1",
      binding: { ...binding, label: "renamed" },
    });
    expect(updated.binding).toMatchObject({ revision: "2", label: "renamed" });

    const list = await client.bindingList();
    expect(list.bindings.map((b) => b.label)).toEqual(["renamed"]);

    const probe = await client.bindingProbe({ binding_id: binding.id });
    expect(probe.binding.id).toBe(binding.id);
    expect(probe.models).toEqual([
      { id: "gpt-6-astra", efforts: [] },
      { id: "gpt-5.6-sol", efforts: [] },
      { id: "gpt-5.6-luna", efforts: [] },
    ]);
    expect(probe.installation).toBe("verified");
    expect(probe.binding.revision).toBe("3");
    expect((await client.bindingList()).bindings[0]).toEqual(probe.binding);
    await expect(client.bindingProbe({ binding_id: "ghost" })).rejects.toMatchObject({ code: "NOT_FOUND" });
  });

  it("filters templates and verification commands by repository", async () => {
    const { client } = newMock();
    const template: TeamTemplate = {
      id: "template-1",
      revision: "0",
      label: "기본 팀",
      repository_id: null,
      role_bindings: [{ role: "lead", primary_binding_id: "binding-1", fallback_binding_ids: [] }],
      policy: fakePolicy(),
    };
    const saved = await client.templateSave({ request_id: "t-1", expected_revision: "0", template });
    expect(saved.template.revision).toBe("1");
    expect((await client.templateList({ repository_id: null })).templates).toHaveLength(1);
    expect((await client.templateList({ repository_id: "repo-x" })).templates).toHaveLength(0);

    const command: VerificationCommand = {
      id: "cmd-1",
      title: "cargo test",
      program: "cargo",
      argv: ["test"],
      revision: "0",
      repository_id: "repo-1",
      cwd_relative: ".",
      timeout_ms: 600000,
      env_profile_ref: null,
      allowed_network: false,
    };
    const savedCommand = await client.verificationSave({ request_id: "v-1", expected_revision: "0", command });
    expect(savedCommand.command.revision).toBe("1");
    expect((await client.verificationList({ repository_id: "repo-1" })).commands.map((c) => c.id)).toEqual(["cmd-1"]);
    expect((await client.verificationList({ repository_id: "repo-x" })).commands).toHaveLength(0);
  });
});

/** 세션이 필요한 시험용 셸 실행(shell 모드는 즉시 세션을 만든다). */
function shellLaunch(requestId: string): LaunchRequest {
  return {
    request_id: requestId,
    profile_id: "shell",
    cwd: "/tmp",
    program: "/bin/zsh",
    argv: [],
    env_overrides: {},
    mode: "shell",
    executor: { kind: "local" },
    cols: 80,
    rows: 24,
    priority: 1,
    policy: {
      reservation_bytes: "0",
      cpu_slots: 1,
      enforcement: "observe",
      memory_max_bytes: null,
      cpu_max_cores: null,
      pids_max: null,
    },
  };
}

// 08-pressure-relief §1: session.focus는 "이 창이 보는 세션"을 기록하고,
// 모르는/끝난 세션은 INVALID_ARGUMENT로 거절한다.
describe("MockDaemonClient session.focus", () => {
  it("records a live session, clears on null, and rejects unknown ids", async () => {
    const { client, drain } = newMock();
    const launched = await client.workloadLaunch(shellLaunch("req-focus-1"));
    drain();
    const sessionId = launched.session_id as string;

    expect(await client.sessionFocus({ session_id: sessionId })).toEqual({
      focused_session_ids: [sessionId],
    });
    expect((await client.systemSnapshot()).focused_session_ids).toEqual([sessionId]);

    expect(await client.sessionFocus({ session_id: null })).toEqual({ focused_session_ids: [] });
    expect((await client.systemSnapshot()).focused_session_ids).toEqual([]);

    await expect(client.sessionFocus({ session_id: "no-such-session" })).rejects.toBeInstanceOf(
      RpcClientError,
    );
    await expect(client.sessionFocus({ session_id: "no-such-session" })).rejects.toMatchObject({
      code: "INVALID_ARGUMENT",
    });
  });

  it("rejects a session whose workload already finished", async () => {
    const { client, drain } = newMock();
    const launched = await client.workloadLaunch(shellLaunch("req-focus-2"));
    drain();
    const sessionId = launched.session_id as string;
    await client.sessionFocus({ session_id: sessionId });

    await client.workloadCancel({ request_id: "cancel-1", workload_id: launched.workload_id });
    drain();

    await expect(client.sessionFocus({ session_id: sessionId })).rejects.toMatchObject({
      code: "INVALID_ARGUMENT",
    });
    // 끝난 세션은 집계에서도 빠진다 — 데몬이 받아 주지 않는 값이다.
    expect((await client.systemSnapshot()).focused_session_ids).toEqual([]);
  });
});

describe("MockDaemonClient resetMissions helper", () => {
  it("wipes the mission store between cases", async () => {
    const { client } = newMock();
    const created = await client.missionCreate(createParams());
    expect(client.missions).toHaveLength(1);

    client.resetMissions();
    expect(client.missions).toHaveLength(0);
    expect((await client.missionList({ cursor: null, limit: 50, archived: false })).items).toHaveLength(0);
    await expect(
      client.missionSnapshot({ mission_id: created.mission_id, snapshot_id: null, cursor: null }),
    ).rejects.toMatchObject({ code: "NOT_FOUND" });
    expect((await client.bindingList()).bindings).toHaveLength(0);
  });
});

// 08-pressure-relief §2: 수동 액션의 뜻(수동 양보는 자동 복원 없음, 수동
// 복원은 압력이 남아 있으면 보호, protect는 복원을 겸한다)과 정책 토글.
describe("MockDaemonClient session.relief", () => {
  it("manual yield marks the workload and shows up in the snapshot", async () => {
    const { client, drain } = newMock();
    const launched = await client.workloadLaunch(shellLaunch("req-relief-1"));
    drain();
    const sessionId = launched.session_id as string;

    const result = await client.sessionRelief({ session_id: sessionId, action: "yield" });
    expect(result.relief).toMatchObject({ kind: "YIELDED", manual: true, partial: false });
    expect(result.protected).toBe(false);

    const snapshot = await client.systemSnapshot();
    const workload = snapshot.workloads.find((w) => w.workload_id === launched.workload_id);
    expect(workload?.relief).toMatchObject({ kind: "YIELDED", manual: true });
  });

  it("restores immediately, and protects for this episode while CPU pressure lasts", async () => {
    const { client, drain } = newMock();
    const launched = await client.workloadLaunch(shellLaunch("req-relief-2"));
    drain();
    const sessionId = launched.session_id as string;
    await client.sessionRelief({ session_id: sessionId, action: "yield" });

    // NORMAL이면 복원만 한다(다음 틱에 다시 양보될 이유가 없다).
    expect(await client.sessionRelief({ session_id: sessionId, action: "restore" })).toEqual({
      relief: { kind: "NONE" },
      protected: false,
    });

    // 압력이 남아 있으면 이번 에피소드 동안 보호한다.
    client.seedCpuPressure("WARNING");
    await client.sessionRelief({ session_id: sessionId, action: "yield" });
    expect(await client.sessionRelief({ session_id: sessionId, action: "restore" })).toEqual({
      relief: { kind: "NONE" },
      protected: true,
    });
  });

  it("protect restores a yielded session and unprotect only clears the flag", async () => {
    const { client, drain } = newMock();
    const launched = await client.workloadLaunch(shellLaunch("req-relief-3"));
    drain();
    const sessionId = launched.session_id as string;
    await client.sessionRelief({ session_id: sessionId, action: "yield" });

    expect(await client.sessionRelief({ session_id: sessionId, action: "protect" })).toEqual({
      relief: { kind: "NONE" },
      protected: true,
    });
    expect(await client.sessionRelief({ session_id: sessionId, action: "unprotect" })).toEqual({
      relief: { kind: "NONE" },
      protected: false,
    });
  });

  it("rejects unknown and finished sessions like session.focus does", async () => {
    const { client, drain } = newMock();
    const launched = await client.workloadLaunch(shellLaunch("req-relief-4"));
    drain();
    const sessionId = launched.session_id as string;

    await expect(client.sessionRelief({ session_id: "no-such-session", action: "yield" })).rejects.toMatchObject({
      code: "INVALID_ARGUMENT",
    });

    await client.workloadCancel({ request_id: "cancel-relief", workload_id: launched.workload_id });
    drain();
    await expect(client.sessionRelief({ session_id: sessionId, action: "yield" })).rejects.toMatchObject({
      code: "INVALID_ARGUMENT",
    });
  });

  it("relief.set_policy stores the flag and reports it in the snapshot", async () => {
    const { client } = newMock();
    expect((await client.systemSnapshot()).relief_policy).toEqual({ auto_yield: true });

    expect(await client.reliefSetPolicy({ auto_yield: false })).toEqual({ auto_yield: false });
    expect((await client.systemSnapshot()).relief_policy).toEqual({ auto_yield: false });

    // 끄는 것만으로 이미 양보된 세션이 복원되지는 않는다.
    const launched = await client.workloadLaunch(shellLaunch("req-relief-5"));
    const sessionId = launched.session_id as string;
    await client.sessionRelief({ session_id: sessionId, action: "yield" });
    await client.reliefSetPolicy({ auto_yield: false });
    const snapshot = await client.systemSnapshot();
    expect(snapshot.workloads.find((w) => w.workload_id === launched.workload_id)?.relief).toMatchObject({
      kind: "YIELDED",
    });
  });
});

const ref = (id: string) => ({ id, sha256: "e".repeat(64), bytes: "8", media_type: "text/plain" });

function fixtureTask(id: string, missionId: string, overrides: Partial<Task> = {}): Task {
  return {
    id, mission_id: missionId, title: "writer", kind: "implement", role: "builder", state: "blocked", required: true,
    parent_task_id: null, depends_on: [],
    contract: { objective_ref: ref("obj"), requirement_ids: [], input_artifact_ids: [], allowed_paths: [], expected_outputs: ["patch"], verification_ids: [], specialty: null },
    binding_id: null, active_run_id: null, ordinal: 1, attempt_count: 1, repair_cycle: 0, failure_repair_run_ids: [],
    integration: null, replacement_of: null, blocked_code: "outcome_unknown", dispatch_after_unix_ms: null, workspace_id: null,
    created_at: "2026-09-17T00:00:00Z", updated_at: "2026-09-17T00:00:00Z", ...overrides,
  };
}

function fixtureRun(id: string, missionId: string, taskId: string, overrides: Partial<Run> = {}): Run {
  return {
    id, mission_id: missionId, task_id: taskId, attempt: 1, state: "unknown", binding_snapshot: null,
    requested_model: null, observed_model: null, provider_session_id: null, provider_turn_id: null, exec_id: null,
    pty_session_id: null, workspace_id: null, fencing_token: "2", dispatch_state: "may_have_sent", context_ref: ref("ctx"),
    result_ref: null, usage: { input_tokens: null, output_tokens: null, cost_usd_micros: null, cost_source: "unknown" },
    last_activity_at: null, active_time_ms: "0", started_at: "2026-09-17T00:00:00Z", ended_at: null,
    failure_code: "OUTCOME_UNKNOWN", reconciliation_ref: null, rate_limit: null, retry_evidence: null, ...overrides,
  };
}

function fixtureWorkspace(id: string, missionId: string, overrides: Partial<Workspace> = {}): Workspace {
  return {
    id, mission_id: missionId, path: `/data/missions/${missionId}/workspaces/${id}`, kind: "worker",
    base_oid: "b".repeat(40), head_oid: "b".repeat(40), writer_run_id: null, lease_token: "1", state: "retained",
    owned_by_daemon: true, ...overrides,
  };
}

describe("MockDaemonClient usability contracts (W1b)", () => {
  it("repository.inspect reports isolated verification support", async () => {
    const { client } = newMock();
    expect(await client.repositoryInspect({ path: "D:/repo/iyagi" })).toMatchObject({ verification_supported: true });
  });

  it("probe records this machine's self-check and opens capabilities without consent (11 §4)", async () => {
    const { client } = newMock();
    const claude: Binding = {
      ...fakeBinding("binding-claude"),
      runtime: "claude",
      program: "/opt/homebrew/bin/claude",
      provider_id: "anthropic",
      model_id: "claude-opus-5",
    };
    await client.bindingSave({ request_id: "exp-1", expected_revision: "0", binding: claude });
    const probe = await client.bindingProbe({ binding_id: claude.id });
    expect(probe.binding.runtime_version).toBe("2.1.271");
    const local = { supported: true, reason_code: "local_probe" };
    const missing = { supported: false, reason_code: "adapter_not_implemented" };
    expect(probe.binding.capabilities).toEqual({
      structured_result: local, events: local, cancel: local, resume: missing, steer: missing,
      approval_reply: missing, read_only: local, scoped_write: local, model_listing: missing,
      usage: local, native_terminal_attach: missing,
    });
    // The report is stored on the connection: protocol proven, no sandbox cases for Claude, and the
    // model this connection names is not one the CLI lists (a warning, not a block).
    expect(probe.binding.local_evidence).toMatchObject({
      os: "macos", version: "2.1.271", model_id: "claude-opus-5",
      probe: { protocol_ok: true, sandbox_cases_passed: null, sandbox_cases_total: null, model_listed: false, failures: [] },
      runs: { succeeded_read_only: 0, succeeded_write: 0, cancelled: 0, invalid_result: 0, last_at: null },
    });
    expect(probe.binding.local_evidence?.probed_at).not.toBeNull();

    // Codex on its pinned model keeps the release evidence and adds the sandbox cases this OS ran.
    const codex = fakeBinding("binding-codex");
    codex.model_id = "gpt-5.6-luna";
    await client.bindingSave({ request_id: "exp-2", expected_revision: "0", binding: codex });
    const codexProbe = await client.bindingProbe({ binding_id: codex.id });
    expect(codexProbe.binding.capabilities.structured_result).toEqual({ supported: true, reason_code: null });
    expect(codexProbe.binding.local_evidence?.probe).toMatchObject({
      protocol_ok: true, sandbox_cases_passed: 12, sandbox_cases_total: 12, model_listed: true,
    });

    // A client cannot write local evidence, and saving the same target keeps what the daemon measured.
    const forged = { ...codexProbe.binding, label: "renamed", local_evidence: null };
    const saved = (await client.bindingSave({ request_id: "exp-3", expected_revision: codexProbe.binding.revision, binding: forged })).binding;
    expect(saved.local_evidence).toEqual(codexProbe.binding.local_evidence);
  });

  it("probe still opens implemented capabilities on a consented connection the self-check cannot reach", async () => {
    const { client } = newMock();
    const refs = { credential_ref: "keyring:11111111-1111-4111-8111-111111111111", endpoint_ref: "11111111-1111-4111-8111-111111111111" };
    // OpenCode is not installed in the fixture, so nothing can be measured here — only the
    // per-connection consent opens what the adapter implements (11 §3.4: no version match).
    const opencode: Binding = {
      ...fakeBinding("binding-opencode"), runtime: "opencode", program: "/opt/homebrew/bin/opencode",
      provider_id: "zai-coding-plan", model_id: "glm-5.3", auth_route: "api_key", ...refs,
      experimental_version: "1.18.0",
    };
    await client.bindingSave({ request_id: "oc-1", expected_revision: "0", binding: opencode });
    const probe = await client.bindingProbe({ binding_id: opencode.id });
    expect(probe.binding.capabilities.structured_result).toEqual({ supported: true, reason_code: "experimental_opt_in" });
    expect(probe.binding.capabilities.model_listing).toEqual({ supported: false, reason_code: "adapter_not_implemented" });
    expect(probe.binding.local_evidence).toBeNull();
    // Without that consent the same connection claims nothing — the probe leaves it as stored.
    const noConsent: Binding = { ...opencode, id: "binding-opencode-2", experimental_version: null };
    await client.bindingSave({ request_id: "oc-2", expected_revision: "0", binding: noConsent });
    const second = await client.bindingProbe({ binding_id: noConsent.id });
    expect(second.binding.capabilities).toEqual(noConsent.capabilities);
  });

  it("follow-up creation requires an accepted mission and its accepted candidate commit", async () => {
    const { client } = newMock();
    const previous = await client.missionCreate(createParams({ request_id: "prev" }));
    const candidate: Candidate = {
      id: "cand-1", mission_id: previous.mission_id, revision: 1, base_oid: "b".repeat(40), tree_oid: "t".repeat(40),
      commit_oid: "c".repeat(40), source_run_ids: [], manifest_ref: ref("manifest"), created_at: "2026-09-17T00:00:00Z", supersedes_id: null,
    };
    const follow = (request: string, base: string) =>
      client.missionCreate(createParams({ request_id: request, expected_base_oid: base, follow_up_of: previous.mission_id }));
    await expect(follow("f-0", "c".repeat(40))).rejects.toMatchObject({
      code: "INVALID_ARGUMENT", details: { reason_code: "follow_up_not_accepted" },
    });
    const accepted = { ...client.missions[0], state: "completed" as const, accepted_at: "2026-09-17T00:00:00Z", candidate_id: candidate.id };
    client.seedMissionEntities(previous.mission_id, [{ kind: "mission", value: accepted }, { kind: "candidate", value: candidate }]);
    await expect(follow("f-1", "b".repeat(40))).rejects.toMatchObject({
      code: "INVALID_ARGUMENT", details: { reason_code: "follow_up_base_mismatch" },
    });
    const created = await follow("f-2", "c".repeat(40));
    const mission = client.missions.find((m) => m.id === created.mission_id);
    expect(mission).toMatchObject({ follow_up_of: previous.mission_id, base_oid: "c".repeat(40) });
    expect(client.missions.find((m) => m.id === previous.mission_id)?.follow_up_of).toBeNull();
  });

  it("attest_exited releases an uncertain run as user_attested and is idempotent", async () => {
    const { client } = newMock();
    const created = await client.missionCreate(createParams({ request_id: "attest-m" }));
    const id = created.mission_id;
    const task = fixtureTask("task-1", id, { active_run_id: "run-1", workspace_id: "ws-1" });
    const run = fixtureRun("run-1", id, task.id, { workspace_id: "ws-1" });
    const workspace = fixtureWorkspace("ws-1", id, { writer_run_id: run.id, state: "busy" });
    const live = fixtureRun("run-live", id, task.id, { state: "running" });
    client.seedMissionEntities(id, [{ kind: "task", value: task }, { kind: "run", value: run }, { kind: "workspace", value: workspace }, { kind: "run", value: live }]);
    const revision = client.missions[0].revision;
    const params = { request_id: "attest-1", mission_id: id, expected_revision: revision, run_id: run.id, attestation: "process_absent_confirmed" };
    await expect(client.missionRunAttestExited({ ...params, request_id: "attest-live", run_id: live.id })).rejects.toMatchObject({
      code: "INVALID_STATE", details: { reason_code: "attestation_not_applicable" },
    });
    await expect(client.missionRunAttestExited({ ...params, request_id: "attest-bad", attestation: "Bad Key" })).rejects.toMatchObject({
      code: "INVALID_ARGUMENT",
    });
    const result = await client.missionRunAttestExited(params);
    expect(await client.missionRunAttestExited(params)).toEqual(result);
    const page = await client.missionSnapshot({ mission_id: id, snapshot_id: null, cursor: null });
    const attested = page.entities.flatMap((e) => (e.kind === "run" && e.value.id === run.id ? [e.value] : []))[0];
    expect(attested.state).toBe("unknown");
    expect(attested.reconciliation_kind).toBe("user_attested");
    expect(attested.reconciliation_ref).not.toBeNull();
    const releasedTask = page.entities.flatMap((e) => (e.kind === "task" && e.value.id === task.id ? [e.value] : []))[0];
    expect(releasedTask).toMatchObject({ active_run_id: null, state: "blocked", blocked_code: "outcome_unknown_ended" });
    const released = page.entities.flatMap((e) => (e.kind === "workspace" ? [e.value] : []))[0];
    expect(released).toMatchObject({ writer_run_id: null, state: "quarantined" });
    // Already attested: no second attestation.
    await expect(client.missionRunAttestExited({ ...params, request_id: "attest-2", expected_revision: result.revision }))
      .rejects.toMatchObject({ details: { reason_code: "attestation_not_applicable" } });
  });

  it("workspace usage/cleanup keep active missions, dirty and leased workspaces", async () => {
    const { client } = newMock();
    const created = await client.missionCreate(createParams({ request_id: "ws-m" }));
    const id = created.mission_id;
    client.seedMissionEntities(id, [
      { kind: "workspace", value: fixtureWorkspace("ws-clean", id) },
      { kind: "workspace", value: fixtureWorkspace("ws-dirty", id, { state: "quarantined" }) },
      { kind: "workspace", value: fixtureWorkspace("ws-leased", id, { writer_run_id: "run-x" }) },
    ]);
    expect((await client.workspaceUsage({ mission_id: id })).missions).toEqual([
      { mission_id: id, workspaces: 3, bytes: String(3 * 1048576), cleanable: false, blocked_reason: "mission_active" },
    ]);
    await expect(client.workspaceCleanup({ request_id: "clean-0", mission_id: id })).rejects.toMatchObject({
      code: "INVALID_STATE", details: { reason_code: "mission_active" },
    });
    client.seedMissionEntities(id, [{ kind: "mission", value: { ...client.missions[0], state: "cancelled" } }]);
    const usage = await client.workspaceUsage({ mission_id: null });
    expect(usage.missions[0]).toMatchObject({ cleanable: true, blocked_reason: null });
    expect(usage.total_bytes).toBe(String(3 * 1048576));
    const cleaned = await client.workspaceCleanup({ request_id: "clean-1", mission_id: id });
    expect(cleaned.removed).toBe(1);
    expect(cleaned.freed_bytes).toBe("1048576");
    expect(cleaned.kept.map((k) => k.reason).sort()).toEqual(["dirty", "run_active"]);
    expect(await client.workspaceCleanup({ request_id: "clean-1", mission_id: id })).toEqual(cleaned);
    expect((await client.workspaceUsage({ mission_id: id })).missions[0]).toMatchObject({ workspaces: 2, bytes: String(2 * 1048576) });
  });
});
