/**
 * 프로필 스토어: CRUD/라벨 중복 가드, localStorage 지속화 round-trip,
 * 기본 시드(03 §8 정확값), JSON 가져오기/내보내기(version 위조 거부).
 */

import { beforeEach, describe, expect, it } from "vitest";
import {
  PROFILES_EXPORT_VERSION,
  PROFILES_STORAGE_KEY,
  createProfilesStore,
  normalizeProfileShape,
  parseEnvelope,
  seedProfiles,
  serializeProfiles,
  type ProfilesStorage,
} from "./profileStore";
import type { LaunchProfile } from "./types";
import { defaultProfilePolicy } from "./types";

class MemoryStorage implements ProfilesStorage {
  map = new Map<string, string>();
  getItem(key: string): string | null {
    return this.map.get(key) ?? null;
  }
  setItem(key: string, value: string): void {
    this.map.set(key, value);
  }
  removeItem(key: string): void {
    this.map.delete(key);
  }
}

let seq = 0;
const uuid = () => `id-${++seq}`;

function makeProfile(label: string, overrides: Partial<LaunchProfile> = {}): LaunchProfile {
  return {
    id: uuid(),
    label,
    descriptor: {
      kind: "custom",
      program: "C:\\Tools\\agent.exe",
      argv_prefix: [],
      detected_version: null,
      transport: "pty",
      capabilities: { turn_events: false, resume: false, concurrency_control: false },
    },
    cwd: "D:\\work",
    policy: defaultProfilePolicy(),
    env: [],
    notes: "",
    interpreter: null,
    ...overrides,
  };
}

beforeEach(() => {
  seq = 0;
});

describe("기본 시드 (04 §5 + 03 §8)", () => {
  it("codex/claude/opencode 세 프로필, program은 감지 전까지 비어 있음", () => {
    const profiles = seedProfiles(uuid);
    expect(profiles.map((p) => p.descriptor.kind)).toEqual(["codex", "claude", "opencode"]);
    for (const p of profiles) {
      expect(p.descriptor.program).toBe("");
      expect(p.descriptor.detected_version).toBeNull();
      expect(p.descriptor.transport).toBe("pty");
      expect(p.descriptor.capabilities).toEqual({
        turn_events: false,
        resume: false,
        concurrency_control: false,
      });
    }
  });

  it("기본 정책은 03 §8 정확값: observe + 2 GiB + cpu_slots 1 + cap 전부 null", () => {
    const [codex] = seedProfiles(uuid);
    expect(codex.policy).toEqual({
      enforcement: "observe",
      reservation_bytes: "2147483648",
      cpu_slots: 1,
      memory_max_bytes: null,
      cpu_max_cores: null,
      pids_max: null,
    });
  });
});

describe("CRUD + 라벨 중복 가드", () => {
  function setup() {
    const storage = new MemoryStorage();
    return { storage, store: createProfilesStore(() => storage, uuid) };
  }

  it("추가·수정·삭제", () => {
    const { store } = setup();
    const added = store.getState().addProfile(makeProfile("내 프로필"));
    expect(added.ok).toBe(true);
    const id = added.ok && added.id ? added.id : "";
    expect(store.getState().profileById(id)?.label).toBe("내 프로필");

    const renamed = store.getState().updateProfile(id, { notes: "메모" });
    expect(renamed.ok).toBe(true);
    expect(store.getState().profileById(id)?.notes).toBe("메모");

    expect(store.getState().removeProfile(id)).toBe(true);
    expect(store.getState().profileById(id)).toBeNull();
    expect(store.getState().removeProfile(id)).toBe(false);
  });

  it("같은 라벨 추가/변경을 거부한다", () => {
    const { store } = setup();
    expect(store.getState().addProfile(makeProfile("claude"))).toEqual({
      ok: false,
      error: expect.stringContaining("claude"),
    });
    const mine = store.getState().addProfile(makeProfile("내 프로필"));
    const myId = mine.ok && mine.id ? mine.id : "";
    const clash = store.getState().updateProfile(myId, { label: "codex" });
    expect(clash.ok).toBe(false);
  });

  it("빈 라벨 거부", () => {
    const { store } = setup();
    expect(store.getState().addProfile(makeProfile("  ")).ok).toBe(false);
  });
});

describe("localStorage 지속화 round-trip (iyagi.profiles.v1)", () => {
  it("변경 즉시 저장되고 같은 storage의 새 스토어가 그대로 읽는다", () => {
    const storage = new MemoryStorage();
    const a = createProfilesStore(() => storage, uuid);
    a.getState().addProfile(makeProfile("round-trip"));
    const added = a.getState().profileByLabel("round-trip");
    expect(added).not.toBeNull();
    a.getState().updateProfile((added as LaunchProfile).id, { notes: "변경" });

    const raw = storage.getItem(PROFILES_STORAGE_KEY);
    expect(raw).toBeTruthy();
    const envelope = JSON.parse(raw as string) as { version: number; profiles: LaunchProfile[] };
    expect(envelope.version).toBe(PROFILES_EXPORT_VERSION);
    expect(envelope.profiles).toHaveLength(4);

    const b = createProfilesStore(() => storage, uuid);
    expect(b.getState().profiles.map((p) => p.label)).toContain("round-trip");
    expect(b.getState().profileByLabel("round-trip")?.notes).toBe("변경");
  });

  it("storage가 없어도 동작하고 손상된 데이터는 시드로 되돌린다", () => {
    const noStorage = createProfilesStore(() => null, uuid);
    expect(noStorage.getState().profiles).toHaveLength(3);

    const broken = new MemoryStorage();
    broken.setItem(PROFILES_STORAGE_KEY, "{not json");
    const recovered = createProfilesStore(() => broken, uuid);
    expect(recovered.getState().profiles).toHaveLength(3);
  });
});

describe("JSON 내보내기/가져오기 (버전 필드 검증)", () => {
  function setup() {
    const storage = new MemoryStorage();
    return { storage, store: createProfilesStore(() => storage, uuid) };
  }

  it("내보낸 JSON은 version 1 + 프로필 배열이고 다시 가져올 수 있다", () => {
    const a = setup();
    const text = a.store.getState().exportJson();
    expect(parseEnvelope(text).version).toBe(1);

    const b = setup();
    const result = b.store.getState().importJson(text);
    expect(result.ok).toBe(true);
    expect(b.store.getState().profiles).toEqual(a.store.getState().profiles);
  });

  it("version이 다르면(위조) 거부한다", () => {
    const { store } = setup();
    const tampered = JSON.stringify({
      version: PROFILES_EXPORT_VERSION + 1,
      profiles: seedProfiles(uuid),
    });
    const result = store.getState().importJson(tampered);
    expect(result.ok).toBe(false);
    if (!result.ok) expect(result.error).toContain("version");
    // 기존 프로필은 유지된다(거부 시 교체 없음).
    expect(store.getState().profiles).toHaveLength(3);
  });

  it("version 없음·잘못된 JSON·불완전한 프로필 형식 거부", () => {
    const { store } = setup();
    expect(store.getState().importJson(JSON.stringify({ profiles: [] })).ok).toBe(false);
    expect(store.getState().importJson("[][]not-json").ok).toBe(false);
    const badShape = JSON.stringify({ version: 1, profiles: [{ id: "x", label: "l" }] });
    expect(store.getState().importJson(badShape).ok).toBe(false);
  });

  it("가져오기 항목 간 라벨 중복 거부", () => {
    const { store } = setup();
    const dup = serializeProfiles([makeProfile("같음"), makeProfile("같음")]);
    expect(store.getState().importJson(dup).ok).toBe(false);
  });

  it("serialize→parse 항등", () => {
    const profiles = seedProfiles(uuid);
    expect(parseEnvelope(serializeProfiles(profiles)).profiles).toEqual(profiles);
  });
});

describe("가져온 프로필의 누락 필드 보충(D#5)", () => {
  const minimal = {
    id: "p-min",
    label: "minimal",
    descriptor: { kind: "custom", program: "/x", argv_prefix: [] },
    policy: { enforcement: "observe" },
    env: [{ key: "A" }],
  };

  it("cwd·notes·interpreter·env 값·정책 수치가 없어도 기본값으로 채워 렌더가 죽지 않는다", () => {
    const text = JSON.stringify({ version: 1, profiles: [minimal] });
    const [profile] = parseEnvelope(text).profiles;
    expect(profile.cwd).toBe("");
    expect(profile.notes).toBe("");
    expect(profile.interpreter).toBeNull();
    expect(profile.env).toEqual([{ key: "A", value: null, secretRef: null }]);
    expect(profile.policy.reservation_bytes).toBe("2147483648");
    expect(profile.policy.cpu_slots).toBe(1);
    expect(profile.descriptor.detected_version).toBeNull();
    expect(profile.descriptor.capabilities).toEqual({ turn_events: false, resume: false, concurrency_control: false });
    // 이전에 `profile.cwd.trim is not a function`으로 죽던 경로.
    expect(() => profile.cwd.trim()).not.toThrow();
  });

  it("있는데 타입이 다른 필드는 거부한다", () => {
    expect(normalizeProfileShape({ ...minimal, interpreter: {} })).toBeNull();
    expect(normalizeProfileShape({ ...minimal, env: [{ key: "A", value: 3 }] })).toBeNull();
    expect(normalizeProfileShape({ ...minimal, cwd: 7 })).toBeNull();
    expect(normalizeProfileShape({ ...minimal, policy: { enforcement: "observe", cpu_slots: 1.5 } })).toBeNull();
  });
});
