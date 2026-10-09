import { describe, expect, it } from "vitest";

import {
  MISSION_PROTOCOL_SUPPORTED,
  missionEntryAvailability,
  missionEntryHidden,
  missionProtocolVersion,
  missionsAvailable,
  missionUnavailableReasonKey,
} from "./capability";

describe("mission feature discovery", () => {
  it("locks missions when the capability is absent", () => {
    expect(missionProtocolVersion({})).toBeNull();
    expect(missionProtocolVersion({ mission_protocol: null })).toBeNull();
    expect(missionProtocolVersion(undefined)).toBeNull();
    expect(missionsAvailable({})).toBe(false);
  });

  it("accepts a matching daemon revision", () => {
    expect(missionProtocolVersion({ mission_protocol: 1 })).toBe(1);
    expect(missionsAvailable({ mission_protocol: 1 })).toBe(true);
    expect(MISSION_PROTOCOL_SUPPORTED).toBe(1);
  });

  it("treats malformed or future revisions as unknown, not downgraded", () => {
    expect(missionProtocolVersion({ mission_protocol: 0 })).toBeNull();
    expect(missionProtocolVersion({ mission_protocol: 1.5 })).toBeNull();
    expect(missionProtocolVersion({ mission_protocol: 99 })).toBe(99);
    expect(missionsAvailable({ mission_protocol: 99 })).toBe(false);
  });
});

describe("mission entry availability", () => {
  it("declared protocol → ready, no reason, never hidden", () => {
    expect(missionEntryAvailability(1)).toBe("ready");
    expect(missionUnavailableReasonKey("ready")).toBeNull();
    expect(missionEntryHidden("ready", true)).toBe(false);
  });

  it("undeclared protocol is a preview: hidden in production, disabled with a reason in development", () => {
    expect(missionEntryAvailability(null)).toBe("preview");
    expect(missionEntryAvailability(undefined)).toBe("preview");
    expect(missionEntryAvailability(0)).toBe("preview");
    expect(missionUnavailableReasonKey("preview")).toBe("missions.newMission.unavailable");
    expect(missionEntryHidden("preview", true)).toBe(true);
    expect(missionEntryHidden("preview", false)).toBe(false);
  });

  it("a daemon newer than the app asks for an app update and stays visible", () => {
    expect(missionEntryAvailability(MISSION_PROTOCOL_SUPPORTED + 1)).toBe("update-app");
    expect(missionUnavailableReasonKey("update-app")).toBe("missions.newMission.updateApp");
    expect(missionEntryHidden("update-app", true)).toBe(false);
  });
});
