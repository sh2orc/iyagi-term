/**
 * 프로필 스토어 (zustand): localStorage 버전 키 `iyagi.profiles.v1`에
 * 지속화한다. CRUD + 라벨 중복 가드 + JSON 내보내기/가져오기(version 필드
 * 위조/불일치 거부) + 세 CLI 기본 시드(program은 감지될 때까지 비어 있음).
 *
 * 비밀은 저장하지 않는다(01 §7): env 항목은 값 또는 OS 보안 저장소 참조
 * 라벨뿐이며 raw 비밀 입력 경로가 아예 없다.
 */

import { create } from "zustand";
import type { CliKind, LaunchProfile, ProfileEnvEntry, ProfileInterpreter } from "./types";
import { DEFAULT_CPU_SLOTS, DEFAULT_RESERVATION_BYTES, defaultCliCapabilities, defaultProfilePolicy } from "./types";
import { t } from "../../i18n";

export const PROFILES_STORAGE_KEY = "iyagi.profiles.v1";
export const PROFILES_EXPORT_VERSION = 1;

/** 최소 localStorage 형태(테스트는 메모리 mock 주입). */
export interface ProfilesStorage {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
  removeItem(key: string): void;
}

export interface ProfileEnvelope {
  version: number;
  profiles: LaunchProfile[];
}

const CLI_KINDS = ["codex", "claude", "opencode", "custom"] as const;

function defaultUuid(): string {
  const g = globalThis as { crypto?: { randomUUID?: () => string } };
  if (g.crypto && typeof g.crypto.randomUUID === "function") return g.crypto.randomUUID();
  return `profile-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 10)}`;
}

export function seedProfiles(uuid: () => string = defaultUuid): LaunchProfile[] {
  const make = (kind: "codex" | "claude" | "opencode"): LaunchProfile => ({
    id: uuid(),
    label: kind,
    descriptor: {
      kind,
      program: "",
      argv_prefix: [],
      detected_version: null,
      transport: "pty",
      capabilities: defaultCliCapabilities(),
    },
    cwd: "",
    policy: defaultProfilePolicy(),
    env: [],
    notes: "",
    interpreter: null,
  });
  return [make("codex"), make("claude"), make("opencode")];
}

// -------------------------------------------------------------- 형식 검증

function isString(v: unknown): v is string {
  return typeof v === "string";
}

/** 가져오기/지속화 복원 시 프로필 최소 형식 검증(위조·불완전 거부). */
export function isValidProfileShape(v: unknown): v is LaunchProfile {
  if (v === null || typeof v !== "object") return false;
  const p = v as Partial<LaunchProfile>;
  if (!isString(p.id) || !isString(p.label)) return false;
  if (!p.descriptor || typeof p.descriptor !== "object") return false;
  if (!(CLI_KINDS as readonly string[]).includes(p.descriptor.kind as string)) return false;
  if (!isString(p.descriptor.program)) return false;
  if (!Array.isArray(p.descriptor.argv_prefix) || !p.descriptor.argv_prefix.every(isString)) return false;
  if (!p.policy || typeof p.policy !== "object") return false;
  if (!["observe", "prefer", "require"].includes(p.policy.enforcement as string)) return false;
  if (!Array.isArray(p.env)) return false;
  return true;
}

function isNullableString(v: unknown): v is string | null {
  return v === null || typeof v === "string";
}

function isNullableNumber(v: unknown): v is number | null {
  return v === null || (typeof v === "number" && Number.isFinite(v));
}

function isStringArray(v: unknown): v is string[] {
  return Array.isArray(v) && v.every(isString);
}

/** env 항목: 누락된 value/secretRef는 null로 보충, 타입 불일치는 거부. */
function normalizeEnvEntry(v: unknown): ProfileEnvEntry | null {
  if (v === null || typeof v !== "object") return null;
  const e = v as { key?: unknown; value?: unknown; secretRef?: unknown };
  const value = e.value ?? null;
  const secretRef = e.secretRef ?? null;
  if (!isString(e.key) || !isNullableString(value) || !isNullableString(secretRef)) return null;
  return { key: e.key, value, secretRef };
}

function normalizeInterpreter(v: unknown): { ok: true; value: ProfileInterpreter | null } | { ok: false } {
  if (v === null || v === undefined) return { ok: true, value: null };
  if (typeof v !== "object") return { ok: false };
  const i = v as { executable?: unknown; scriptArgvPrefix?: unknown };
  if (!isString(i.executable) || !isStringArray(i.scriptArgvPrefix)) return { ok: false };
  return { ok: true, value: { executable: i.executable, scriptArgvPrefix: i.scriptArgvPrefix } };
}

/**
 * 형식 검증 + 누락 필드 기본값 보충. 가져오기/복원 파일에서 선택 필드
 * (cwd·notes·interpreter·env 값·정책 수치)가 빠져 있으면 기본값으로 채우고,
 * 있는데 타입이 다르면(env 값이 숫자, interpreter가 빈 객체 등) null —
 * 렌더 중 `.trim()`/`.includes()` 예외로 실행 패널이 죽지 않게 한다.
 */
export function normalizeProfileShape(v: unknown): LaunchProfile | null {
  if (!isValidProfileShape(v)) return null;
  const p = v as unknown as Record<string, unknown>;
  const d = p.descriptor as Record<string, unknown>;
  const policy = p.policy as Record<string, unknown>;
  const cwd = p.cwd ?? "";
  const notes = p.notes ?? "";
  if (!isString(cwd) || !isString(notes)) return null;
  const interpreter = normalizeInterpreter(p.interpreter);
  if (!interpreter.ok) return null;
  const env: Array<ProfileEnvEntry | null> = (p.env as unknown[]).map(normalizeEnvEntry);
  if (env.some((e) => e === null)) return null;
  const detectedVersion = d.detected_version ?? null;
  if (!isNullableString(detectedVersion)) return null;
  const caps = d.capabilities ?? defaultCliCapabilities();
  if (caps === null || typeof caps !== "object") return null;
  const c = caps as Record<string, unknown>;
  const reservation = policy.reservation_bytes ?? DEFAULT_RESERVATION_BYTES;
  const cpuSlots = policy.cpu_slots ?? DEFAULT_CPU_SLOTS;
  const memoryMax = policy.memory_max_bytes ?? null;
  const cpuMax = policy.cpu_max_cores ?? null;
  const pidsMax = policy.pids_max ?? null;
  if (
    !isString(reservation) ||
    typeof cpuSlots !== "number" ||
    !Number.isInteger(cpuSlots) ||
    !isNullableString(memoryMax) ||
    !isNullableNumber(cpuMax) ||
    !isNullableNumber(pidsMax)
  ) {
    return null;
  }
  return {
    id: p.id as string,
    label: p.label as string,
    descriptor: {
      kind: d.kind as CliKind,
      program: d.program as string,
      argv_prefix: d.argv_prefix as string[],
      detected_version: detectedVersion,
      transport: "pty",
      capabilities: {
        turn_events: c.turn_events === true,
        resume: c.resume === true,
        concurrency_control: c.concurrency_control === true,
      },
    },
    cwd,
    policy: {
      enforcement: policy.enforcement as LaunchProfile["policy"]["enforcement"],
      reservation_bytes: reservation,
      cpu_slots: cpuSlots,
      memory_max_bytes: memoryMax,
      cpu_max_cores: cpuMax,
      pids_max: pidsMax,
    },
    env: env as ProfileEnvEntry[],
    notes,
    interpreter: interpreter.value,
  };
}

export function parseEnvelope(text: string): ProfileEnvelope {
  const raw = JSON.parse(text) as { version?: unknown; profiles?: unknown };
  if (raw === null || typeof raw !== "object") throw new Error(t("profile.error.notObject"));
  if (raw.version !== PROFILES_EXPORT_VERSION) {
    throw new Error(
      t("profile.error.unsupportedVersion", { actual: String(raw.version), expected: PROFILES_EXPORT_VERSION }),
    );
  }
  if (!Array.isArray(raw.profiles)) throw new Error(t("profile.error.noProfilesArray"));
  const profiles: LaunchProfile[] = [];
  for (const p of raw.profiles) {
    const normalized = normalizeProfileShape(p);
    if (!normalized) throw new Error(t("profile.error.badShape"));
    profiles.push(normalized);
  }
  return { version: PROFILES_EXPORT_VERSION, profiles };
}

export function serializeProfiles(profiles: LaunchProfile[]): string {
  const envelope: ProfileEnvelope = { version: PROFILES_EXPORT_VERSION, profiles };
  return JSON.stringify(envelope, null, 2);
}

// ------------------------------------------------------------------ 스토어

export type ProfileMutationResult = { ok: true; id?: string } | { ok: false; error: string };

export interface ProfilesState {
  profiles: LaunchProfile[];
  addProfile(input: Omit<LaunchProfile, "id"> & { id?: string }): ProfileMutationResult;
  updateProfile(id: string, patch: Partial<Omit<LaunchProfile, "id">>): ProfileMutationResult;
  removeProfile(id: string): boolean;
  resetToDefaults(): void;
  exportJson(): string;
  importJson(text: string): ProfileMutationResult;
  profileById(id: string | null): LaunchProfile | null;
  profileByLabel(label: string): LaunchProfile | null;
}

export type ProfilesStore = ReturnType<typeof createProfilesStore>;

export function createProfilesStore(
  getStorage: () => ProfilesStorage | null,
  uuid: () => string = defaultUuid,
) {
  const loadInitial = (): LaunchProfile[] => {
    try {
      const storage = getStorage();
      const raw = storage?.getItem(PROFILES_STORAGE_KEY) ?? null;
      if (raw === null) return seedProfiles(uuid);
      const { profiles } = parseEnvelope(raw);
      return profiles;
    } catch {
      return seedProfiles(uuid);
    }
  };

  const persist = (profiles: LaunchProfile[]): void => {
    try {
      getStorage()?.setItem(PROFILES_STORAGE_KEY, serializeProfiles(profiles));
    } catch {
      // 저장 실패는 세션 내 상태를 유지한 채 무시한다.
    }
  };

  return create<ProfilesState>((set, get) => ({
    profiles: loadInitial(),

    addProfile: (input) => {
      const label = input.label.trim();
      if (!label) return { ok: false, error: t("profile.error.labelRequired") };
      if (get().profiles.some((p) => p.label === label)) {
        return { ok: false, error: t("profile.error.duplicate", { label }) };
      }
      const profile: LaunchProfile = { ...input, label, id: input.id ?? uuid() };
      const profiles = [...get().profiles, profile];
      set({ profiles });
      persist(profiles);
      return { ok: true, id: profile.id };
    },

    updateProfile: (id, patch) => {
      const current = get().profiles.find((p) => p.id === id);
      if (!current) return { ok: false, error: t("profile.error.notFound") };
      const label = (patch.label ?? current.label).trim();
      if (!label) return { ok: false, error: t("profile.error.labelRequired") };
      if (get().profiles.some((p) => p.id !== id && p.label === label)) {
        return { ok: false, error: t("profile.error.duplicate", { label }) };
      }
      const profiles = get().profiles.map((p) => (p.id === id ? { ...p, ...patch, label, id } : p));
      set({ profiles });
      persist(profiles);
      return { ok: true, id };
    },

    removeProfile: (id) => {
      if (!get().profiles.some((p) => p.id === id)) return false;
      const profiles = get().profiles.filter((p) => p.id !== id);
      set({ profiles });
      persist(profiles);
      return true;
    },

    resetToDefaults: () => {
      const profiles = seedProfiles(uuid);
      set({ profiles });
      persist(profiles);
    },

    exportJson: () => serializeProfiles(get().profiles),

    importJson: (text) => {
      let envelope: ProfileEnvelope;
      try {
        envelope = parseEnvelope(text);
      } catch (error) {
        return { ok: false, error: error instanceof Error ? error.message : t("profile.error.importFailed") };
      }
      const labels = new Set<string>();
      for (const p of envelope.profiles) {
        if (labels.has(p.label)) return { ok: false, error: t("profile.error.importDuplicate", { label: p.label }) };
        labels.add(p.label);
      }
      set({ profiles: envelope.profiles });
      persist(envelope.profiles);
      return { ok: true };
    },

    profileById: (id) => (id ? get().profiles.find((p) => p.id === id) ?? null : null),

    profileByLabel: (label) => get().profiles.find((p) => p.label === label) ?? null,
  }));
}

function browserStorage(): ProfilesStorage | null {
  try {
    const w = globalThis as { localStorage?: ProfilesStorage };
    return w.localStorage ?? null;
  } catch {
    return null;
  }
}

/** 앱 기본 스토어 — localStorage(없으면 세션 내 상태만). */
export const useProfilesStore = createProfilesStore(browserStorage);
