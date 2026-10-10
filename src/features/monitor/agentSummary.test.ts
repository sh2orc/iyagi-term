import { describe, expect, it } from "vitest";
import type { AgentStatus } from "../../generated/AgentStatus";
import type { PaneMeta } from "../../store/workbenchStore";
import { summarizeAgents } from "./agentSummary";

type PaneInput = Pick<PaneMeta, "phase" | "agent" | "sessionId">;

function pane(agent: Partial<AgentStatus> | null, overrides: Partial<PaneInput> = {}): PaneInput {
  return {
    phase: "live",
    sessionId: `s-${Math.random()}`,
    agent: agent ? { agent: "claude", pid: 1, detected_at_ms: 1, ...agent } : null,
    ...overrides,
  };
}

describe("summarizeAgents", () => {
  it("counts each detected agent kind, with GLM-routed Claude shown as Z.ai", () => {
    const summary = summarizeAgents([
      pane({ agent: "codex" }),
      pane({ agent: "claude", model: "opus" }),
      pane({ agent: "claude", model: "glm-5.3" }),
      pane({ agent: "claude" }),
      pane(null), // a plain shell
    ], new Set());
    expect(summary.total).toBe(4);
    expect(summary.kinds.map((kind) => [kind.id, kind.name, kind.count])).toEqual([
      ["claude", "Claude Code", 2],
      ["zai", "Z.ai", 1],
      ["codex", "Codex", 1],
    ]);
  });

  it("counts only live panes", () => {
    const summary = summarizeAgents([
      pane({ agent: "claude" }, { phase: "exited" }),
      pane({ agent: "codex" }, { phase: "replaying" }),
      pane({ agent: "opencode" }),
    ], new Set());
    expect(summary.total).toBe(1);
    expect(summary.kinds.map((kind) => kind.id)).toEqual(["opencode"]);
  });

  it("splits working and waiting by self-reported status, falling back to recent output", () => {
    const quiet = pane({ agent: "codex" }, { sessionId: "s-quiet" });
    const streaming = pane({ agent: "codex" }, { sessionId: "s-streaming" });
    const summary = summarizeAgents([
      pane({ agent: "claude", session_status: "busy" }),
      pane({ agent: "claude", session_status: "waiting" }),
      pane({ agent: "claude", session_status: "idle" }, { sessionId: "s-streaming-idle" }),
      quiet,
      streaming,
    ], new Set(["s-streaming", "s-streaming-idle"]));
    // claude busy + codex streaming = working; the idle self-report wins over output.
    expect(summary.working).toBe(2);
    expect(summary.waiting).toBe(1);
  });

  it("is empty when no terminal runs an agent", () => {
    expect(summarizeAgents([pane(null)], new Set())).toEqual({ kinds: [], total: 0, working: 0, waiting: 0 });
  });
});
