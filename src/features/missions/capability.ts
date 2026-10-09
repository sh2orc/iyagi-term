/**
 * O1 mission feature discovery (ticket O01).
 *
 * The daemon advertises `capabilities.mission_protocol` (number) only while
 * the mission feature gate is on. Absent or null means the UI must lock
 * mission creation — plain terminal tabs keep working unchanged. The declared
 * revision is a minimum: a client that only speaks revision 1 treats an
 * unknown higher revision as "unsupported", not as "latest".
 */

/** Minimum protocol revision this frontend implements. */
export const MISSION_PROTOCOL_SUPPORTED = 1;

export interface MissionCapabilitySource {
  mission_protocol?: number | null;
}

/**
 * Returns the daemon-supported mission protocol revision, or null when the
 * daemon does not serve missions. A daemon advertising a higher revision than
 * this build speaks is reported as null (unknown to us), never silently
 * downgraded.
 */
export function missionProtocolVersion(
  capabilities: MissionCapabilitySource | null | undefined,
): number | null {
  const advertised = capabilities?.mission_protocol ?? null;
  if (advertised === null) return null;
  if (!Number.isInteger(advertised) || advertised < 1) return null;
  return advertised;
}

/** True only when the daemon and this frontend share a usable protocol. */
export function missionsAvailable(
  capabilities: MissionCapabilitySource | null | undefined,
): boolean {
  const version = missionProtocolVersion(capabilities);
  return version !== null && version <= MISSION_PROTOCOL_SUPPORTED;
}

/**
 * 진입점(상단 버튼·팔레트·메뉴·단축키·pane 메뉴)이 보일 모습.
 *
 * - `ready`: 데몬과 앱이 같은 프로토콜을 쓴다.
 * - `preview`: 데몬이 프로토콜을 선언하지 않았다 — AI 작업은 개발 빌드에서만
 *   켤 수 있는 미리보기다. 프로덕션 빌드에서는 진입점을 숨긴다(`hidden`).
 * - `update-app`: 데몬 프로토콜이 이 앱보다 새롭다 — 숨기지 않고 앱 업데이트를
 *   안내한다(빌드 종류와 무관).
 */
export type MissionEntryAvailability = "ready" | "preview" | "update-app";

export function missionEntryAvailability(protocol: number | null | undefined): MissionEntryAvailability {
  const version = missionProtocolVersion({ mission_protocol: protocol ?? null });
  if (version === null) return "preview";
  return version <= MISSION_PROTOCOL_SUPPORTED ? "ready" : "update-app";
}

/** 비활성 사유 문구의 i18n 키(사용 가능하면 null). */
export function missionUnavailableReasonKey(availability: MissionEntryAvailability): string | null {
  switch (availability) {
    case "ready":
      return null;
    case "preview":
      return "missions.newMission.unavailable";
    case "update-app":
      return "missions.newMission.updateApp";
  }
}

/**
 * 프로덕션 빌드인가 — Vite가 빌드 시점에 상수로 바꾼다(App.tsx의 dev harness와
 * 같은 구분). 시험 환경(vitest)은 개발 빌드로 본다.
 */
export const PRODUCTION_BUILD: boolean = import.meta.env.PROD === true;

/** 진입점을 아예 숨기는가: 프로덕션 빌드에서 데몬이 프로토콜을 선언하지 않았을 때만. */
export function missionEntryHidden(
  availability: MissionEntryAvailability,
  production: boolean = PRODUCTION_BUILD,
): boolean {
  return production && availability === "preview";
}

export interface MissionEntryState {
  availability: MissionEntryAvailability;
  /** 진입점을 그리지 않는다(프로덕션 빌드 + 프로토콜 없음). */
  hidden: boolean;
  /** 누를 수 있다. */
  enabled: boolean;
  /** 비활성 사유 i18n 키(사용 가능하면 null). */
  reasonKey: string | null;
}

/** 진입점 하나가 보일 모습(숨김·활성·사유)을 한 번에. */
export function missionEntryState(
  protocol: number | null | undefined,
  production: boolean = PRODUCTION_BUILD,
): MissionEntryState {
  const availability = missionEntryAvailability(protocol);
  return {
    availability,
    hidden: missionEntryHidden(availability, production),
    enabled: availability === "ready",
    reasonKey: missionUnavailableReasonKey(availability),
  };
}
