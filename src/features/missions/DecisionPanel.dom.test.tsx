/**
 * DecisionPanel 시험(05 §8): 답변 흐름(fake client) · obsolete 비활성.
 */

import { act } from "react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { DecisionPanel } from "./DecisionPanel";
import {
  click,
  compatibleBinding,
  dispatch,
  fakeDecision,
  fakeMission,
  fakeRun,
  fakeTask,
  flushAsync,
  installMockClient,
  pressKey,
  renderUi,
  resetAllMissionState,
  seedStore,
  setValue,
  uploadMockText,
} from "./testSupport";
import { MissionDecisionAnswerParams } from "../../generated/MissionDecisionAnswerParams";
import type { ArtifactRef } from "../../generated/ArtifactRef";
import type { Mission } from "../../generated/Mission";
import type { MockDaemonClient } from "../daemon/mockClient";
import { RpcClientError } from "../daemon/client";
import { t } from "../../i18n";
import { useWorkbenchStore } from "../../store/workbenchStore";
import { useMissionStore } from "./store";

beforeEach(() => {
  resetAllMissionState();
});

it.each([true, false])("파일 변경 내역의 확인 여부(%s)에 따라 승인하고 범위·diff를 텍스트로 표시한다", async available => {
  const client = installMockClient();
  const details = { type: "file_change", reason: "Requested update", grant_root: "/outside", details_available: available,
    changes: available ? [{ path: "old.txt", kind: { type: "update", move_path: "new.txt" }, diff: "+<script>untrusted()</script>" }] : null };
  const ref = await uploadMockText(client, JSON.stringify({ provider_request_id: "private-provider-id", question: JSON.stringify(details) }));
  const mission = fakeMission();
  const decision = fakeDecision(mission.id, { kind: "approval", question_ref: ref, options: [{id:"accept",label:"Allow"},{id:"decline",label:"Deny"}] });
  seedStore({ missions: [mission], decisions: [decision] });
  const handle = renderUi(<DecisionPanel mission={mission} decision={decision} currentDecisionId={null} onJumpToCurrent={()=>{}} />);
  try {
    await flushAsync();
    expect(handle.container.textContent).toContain("/outside");
    expect(handle.container.textContent).not.toContain("private-provider-id");
    const buttons = handle.container.querySelectorAll<HTMLButtonElement>("[data-testid=decision-option]");
    expect(buttons[0].disabled).toBe(!available);
    expect(buttons[1].disabled).toBe(false);
    if (available) {
      expect(handle.container.textContent).toContain("old.txt");
      expect(handle.container.textContent).toContain("new.txt");
      expect(handle.container.querySelector("pre")?.textContent).toContain("<script>untrusted()</script>");
      expect(handle.container.querySelector("script")).toBeNull();
    } else expect(handle.container.textContent).toContain("전체 변경 내역을 확인할 수 없어");
  } finally { handle.unmount(); }
});

describe("DecisionPanel 답변 흐름", () => {
  it("선택지 클릭 → mission.decision.answer(option_id) 호출", async () => {
    const client = installMockClient();
    const questionRef = await uploadMockText(client, "기존 API 호환 범위를 선택해 주세요.");
    const mission = fakeMission();
    const decision = fakeDecision(mission.id, { question_ref: questionRef });
    seedStore({ missions: [mission], decisions: [decision] });

    const answerSpy = vi.fn(async (_params: MissionDecisionAnswerParams) => {
      throw new Error("not hitting the mock store — spy only");
    });
    client.missionDecisionAnswer = answerSpy;

    const handle = renderUi(
      <DecisionPanel mission={mission} decision={decision} currentDecisionId={null} onJumpToCurrent={() => undefined} />,
    );
    await flushAsync();
    expect(handle.container.querySelector("[data-testid='decision-question']")?.textContent).toContain(
      "기존 API 호환 범위",
    );
    const options = handle.container.querySelectorAll("[data-testid='decision-option']");
    expect(options.length).toBe(2);
    click(options[0]);
    await flushAsync();
    expect(answerSpy).toHaveBeenCalledTimes(1);
    expect(answerSpy.mock.calls[0][0]).toMatchObject({
      mission_id: mission.id,
      decision_id: decision.id,
      option_id: "keep",
      expected_revision: mission.revision,
    });
    handle.unmount();
  });

  it("실제 mock store를 통과하면 답변 후 버튼이 비활성된다", async () => {
    const client = installMockClient();
    const questionRef = await uploadMockText(client, "질문");
    // mock에 mission을 만들고 decision을 심는다(UI store도 같은 값으로).
    const created = await client.missionCreate({
      request_id: "req-dp-1",
      title: "로그인 기능",
      repository_path: "/repo",
      expected_base_oid: "",
      goal_ref: questionRef,
      requirements: [],
      policy: {
        max_parallel_runs: 1,
        max_attempts_per_task: 1,
        max_repair_cycles: 1,
        max_automatic_starts: 1,
        active_time_limit_ms: "86400000",
        run_time_limit_ms: "3600000",
        max_cost_usd_micros: null,
        unknown_cost: "block",
        allow_network: false,
        allow_automatic_plan_apply: false,
        allow_recovery_of_unsent: false,
        allowed_binding_ids: [],
        allowed_roles: [],
        allowed_verification_ids: [],
        require_independent_review: false,
        require_enforced_verification: false,
      },
      role_bindings: [],
    });
    const decision = fakeDecision(created.mission_id, { question_ref: questionRef });
    client.seedMissionEntities(created.mission_id, [{ kind: "decision", value: decision }]);
    const mission = client.missions[0];
    seedStore({ missions: [mission], decisions: [decision] });

    const handle = renderUi(
      <DecisionPanel mission={mission} decision={decision} currentDecisionId={null} onJumpToCurrent={() => undefined} />,
    );
    click(handle.container.querySelectorAll("[data-testid='decision-option']")[1]);
    await flushAsync();
    const snapshot = await client.missionSnapshot({ mission_id: created.mission_id, snapshot_id: null, cursor: null });
    const answered = snapshot.entities.find((entity) => entity.kind === "decision");
    expect(answered && answered.kind === "decision" ? answered.value.state : null).toBe("answered");
    expect(
      answered && answered.kind === "decision" ? answered.value.selected_option_id : null,
    ).toBe("breaking");
    handle.unmount();
  });
});

describe("DecisionPanel obsolete", () => {
  it("obsolete 질문은 버튼이 비활성되고 현재 질문 링크가 있다", () => {
    const mission = fakeMission();
    const obsolete = fakeDecision(mission.id, { state: "obsolete" });
    seedStore({ missions: [mission], decisions: [obsolete] });
    const handle = renderUi(
      <DecisionPanel
        mission={mission}
        decision={obsolete}
        currentDecisionId="current-1"
        onJumpToCurrent={() => undefined}
      />,
    );
    const option = handle.container.querySelector("[data-testid='decision-option']") as HTMLButtonElement;
    expect(option.disabled).toBe(true);
    expect(handle.container.textContent).toContain("이 질문은 이미 처리되었습니다.");
    expect(handle.container.textContent).toContain("현재 질문 열기");
    handle.unmount();
  });
});

// ---------------------------------------------------------------- 새 형식 · 입력 · 행동

const mutationResult = (mission: Mission) => ({ mission_id: mission.id, revision: "6", event_seq: "6", entity_ids: [] });

/** artifact 업로드 3단계를 대역으로 바꾸고 commit이 돌려줄 ref를 준다. */
function stubUpload(client: MockDaemonClient): { ref: ArtifactRef; begin: ReturnType<typeof vi.fn> } {
  const ref: ArtifactRef = { id: "artifact-answer", sha256: "0".repeat(64), bytes: "12", media_type: "text/plain; charset=utf-8" };
  const begin = vi.fn(async () => ({ upload_id: "upload-1", chunk_bytes: 4096 }));
  client.artifactBegin = begin;
  client.artifactWrite = vi.fn(async () => ({ next_offset: "12" }));
  client.artifactCommit = vi.fn(async () => ref);
  return { ref, begin };
}

/** syncMission 대역(revision 갱신) — 반환 함수로 원래 구현을 되돌린다. */
function stubSync(mission: Mission, nextRevision: string): { sync: ReturnType<typeof vi.fn>; restore: () => void } {
  const original = useMissionStore.getState().syncMission;
  const sync = vi.fn(async () => {
    useMissionStore.setState((s) => ({ missions: { ...s.missions, [mission.id]: { ...mission, revision: nextRevision } } }));
  });
  useMissionStore.setState({ syncMission: sync });
  return { sync, restore: () => useMissionStore.setState({ syncMission: original }) };
}

describe("DecisionPanel 형식", () => {
  it("상황 1문장 → 선택지(누르면 생기는 일 1줄) → 자세히 순서이고 낭독 영역을 만들지 않는다", () => {
    const mission = fakeMission();
    const task = fakeTask(mission.id, "blocked", { blocked_code: "outcome_unknown_ended", active_run_id: null });
    const run = fakeRun(mission.id, task, "unknown");
    run.reconciliation_ref = run.context_ref;
    const decision = fakeDecision(mission.id, { kind: "recovery", requesting_run_id: run.id, affected_task_ids: [task.id],
      options: [{ id: "retry_reconciled_task", label: "Host retry" }, { id: "stop_reconciled_mission", label: "Host stop" }] });
    seedStore({ missions: [mission], tasks: [task], runs: [run], decisions: [decision] });
    const handle = renderUi(<DecisionPanel mission={mission} decision={decision} currentDecisionId={null} onJumpToCurrent={() => undefined} />);
    try {
      const panel = handle.container.querySelector("[data-testid=decision-panel]")!;
      expect(panel.querySelector("[aria-live]")).toBeNull();
      expect(panel.querySelector("[role=status]")).toBeNull();
      const question = panel.querySelector("[data-testid=decision-question]")!;
      const footer = panel.querySelector("[data-testid=decision-footer]")!;
      const details = panel.querySelector("[data-testid=decision-details]")!;
      expect(question.compareDocumentPosition(footer) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
      expect(footer.compareDocumentPosition(details) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
      expect(details.tagName).toBe("DETAILS");
      expect(details.hasAttribute("open")).toBe(false);
      const params = { task: task.title, attempt: task.attempt_count, limit: mission.policy.max_attempts_per_task };
      expect(question.textContent).toBe(t("missions.decision.situation.reconciled", params));
      expect(details.textContent).toContain(t("missions.recovery.question", params));
      const options = [...panel.querySelectorAll<HTMLButtonElement>("[data-testid=decision-option]")];
      expect(options.map((option) => option.textContent)).toEqual([t("missions.recovery.retry"), t("missions.recovery.stop")]);
      for (const option of options) {
        expect(footer.contains(option)).toBe(true);
        const effect = document.getElementById(option.getAttribute("aria-describedby")!);
        expect(effect?.textContent).toBe(t(`missions.decision.effect.${option.dataset.optionId}`));
      }
    } finally {
      handle.unmount();
    }
  });

  it("계획 결정은 제안 JSON을 할 일 목록(제목·역할·선행 작업)으로 보여 준다", async () => {
    const client = installMockClient();
    const rationale = await uploadMockText(client, "API보다 스키마를 먼저 정합니다.");
    const proposal = {
      id: "proposal-1", mission_id: "mission", based_on_plan_revision: 1, retire_task_ids: [], rationale_ref: rationale,
      tasks: [
        { id: "p-schema", title: "스키마 설계", kind: "design", role: "architect", required: true, parent_task_id: null, depends_on: [], binding_id: null, replacement_of: null },
        { id: "p-api", title: "API 구현", kind: "implement", role: "builder", required: false, parent_task_id: null, depends_on: ["p-schema"], binding_id: null, replacement_of: null },
      ],
    };
    const ref = await uploadMockText(client, JSON.stringify(proposal));
    const mission = fakeMission();
    const decision = fakeDecision(mission.id, { kind: "plan", question_ref: ref,
      options: [{ id: "apply", label: "Apply plan" }, { id: "revise", label: "Revise plan" }] });
    seedStore({ missions: [mission], decisions: [decision] });
    const handle = renderUi(<DecisionPanel mission={mission} decision={decision} currentDecisionId={null} onJumpToCurrent={() => undefined} />);
    try {
      await flushAsync();
      expect(handle.container.querySelector("[data-testid=decision-question]")?.textContent).toBe(t("missions.decision.situation.plan", { count: 2 }));
      const items = [...handle.container.querySelectorAll("[data-testid=decision-plan-task]")].map((item) => item.textContent ?? "");
      expect(items).toHaveLength(2);
      expect(items[0]).toContain("스키마 설계");
      expect(items[0]).toContain(t("missions.role.architect"));
      expect(items[1]).toContain("API 구현");
      expect(items[1]).toContain(t("missions.decision.plan.dependsOn", { titles: "스키마 설계" }));
      expect(items[1]).toContain(t("missions.decision.plan.optional"));
      expect(handle.container.textContent).not.toContain("p-schema");
      expect(handle.container.querySelector("[data-testid=decision-details]")?.textContent).toContain("API보다 스키마를 먼저 정합니다.");
      expect([...handle.container.querySelectorAll("[data-testid=decision-option]")].map((b) => b.textContent))
        .toEqual([t("missions.decision.option.apply"), t("missions.decision.option.revise")]);
    } finally {
      handle.unmount();
    }
  });

  it("계획 본문을 해석하지 못하면 짧은 상황 문장과 원문(자세히)을 보여 준다", async () => {
    const client = installMockClient();
    const ref = await uploadMockText(client, "1. schema\n2. api");
    const mission = fakeMission();
    const decision = fakeDecision(mission.id, { kind: "plan", question_ref: ref,
      options: [{ id: "apply", label: "Apply plan" }, { id: "revise", label: "Revise plan" }] });
    seedStore({ missions: [mission], decisions: [decision] });
    const handle = renderUi(<DecisionPanel mission={mission} decision={decision} currentDecisionId={null} onJumpToCurrent={() => undefined} />);
    try {
      await flushAsync();
      expect(handle.container.querySelector("[data-testid=decision-question]")?.textContent).toBe(t("missions.decision.situation.planUnparsed"));
      expect(handle.container.querySelector("[data-testid=decision-plan]")).toBeNull();
      expect(handle.container.querySelector("[data-testid=decision-original]")?.textContent).toContain("1. schema");
    } finally {
      handle.unmount();
    }
  });

  it("영향 작업은 제목 목록이고 누르면 해당 할 일을 연다", () => {
    const mission = fakeMission();
    const first = fakeTask(mission.id, "blocked", { title: "스키마 설계" });
    const second = fakeTask(mission.id, "ready", { title: "API 구현" });
    const decision = fakeDecision(mission.id, { affected_task_ids: [first.id, second.id] });
    seedStore({ missions: [mission], tasks: [first, second], decisions: [decision] });
    const onOpenTask = vi.fn();
    let handle = renderUi(<DecisionPanel mission={mission} decision={decision} currentDecisionId={null} onJumpToCurrent={() => undefined} onOpenTask={onOpenTask} />);
    const wrap = handle.container.querySelector("[data-testid=decision-affected]")!;
    expect(wrap.textContent).toContain(t("missions.decision.affected", { count: 2 }));
    const links = [...wrap.querySelectorAll("[data-testid=decision-affected-task]")];
    expect(links.map((link) => link.textContent)).toEqual(["스키마 설계", "API 구현"]);
    expect(links.every((link) => link.tagName === "BUTTON")).toBe(true);
    click(links[1]);
    expect(onOpenTask).toHaveBeenCalledWith(second.id);
    expect(wrap.textContent).not.toContain(first.id);
    handle.unmount();
    handle = renderUi(<DecisionPanel mission={mission} decision={decision} currentDecisionId={null} onJumpToCurrent={() => undefined} />);
    expect([...handle.container.querySelectorAll("[data-testid=decision-affected-task]")].every((link) => link.tagName === "SPAN")).toBe(true);
    handle.unmount();
  });
});

describe("DecisionPanel 자유 입력", () => {
  it("계획 수정 요청은 입력한 의견을 artifact로 올려 answer_ref로 보낸다(적용은 본문 없이)", async () => {
    const client = installMockClient();
    const question = await uploadMockText(client, "plain plan");
    const mission = fakeMission();
    const decision = fakeDecision(mission.id, { kind: "plan", question_ref: question,
      options: [{ id: "apply", label: "Apply plan" }, { id: "revise", label: "Revise plan" }] });
    seedStore({ missions: [mission], decisions: [decision] });
    const { ref, begin } = stubUpload(client);
    const answer = vi.spyOn(client, "missionDecisionAnswer").mockResolvedValue(mutationResult(mission));
    const handle = renderUi(<DecisionPanel mission={mission} decision={decision} currentDecisionId={null} onJumpToCurrent={() => undefined} />);
    try {
      await flushAsync();
      const option = (id: string) => handle.container.querySelector<HTMLButtonElement>(`[data-option-id=${id}]`)!;
      click(option("apply"));
      await flushAsync();
      expect(begin).not.toHaveBeenCalled();
      expect(answer.mock.calls[0][0]).toMatchObject({ option_id: "apply", answer_ref: null });
      const textarea = handle.container.querySelector<HTMLTextAreaElement>("[data-testid=decision-text]")!;
      setValue(textarea, "  테스트 작업을 먼저 넣어 주세요  ");
      click(option("revise"));
      await flushAsync();
      expect(begin.mock.calls[0][0]).toMatchObject({ mission_id: mission.id, bytes: String(new TextEncoder().encode("테스트 작업을 먼저 넣어 주세요").byteLength) });
      expect(answer.mock.calls[1][0]).toMatchObject({ decision_id: decision.id, option_id: "revise", answer_ref: ref });
      expect(textarea.value).toBe("");
    } finally {
      handle.unmount();
    }
  });

  it("선택지 없는 질문은 답변 입력이 필수이고 IME 조합 중 Enter로는 보내지 않는다", async () => {
    const client = installMockClient();
    const question = await uploadMockText(client, "배포 대상 환경 이름을 알려 주세요.");
    const mission = fakeMission();
    const decision = fakeDecision(mission.id, { kind: "product", question_ref: question, options: [] });
    seedStore({ missions: [mission], decisions: [decision] });
    const { ref } = stubUpload(client);
    const answer = vi.spyOn(client, "missionDecisionAnswer").mockResolvedValue(mutationResult(mission));
    const handle = renderUi(<DecisionPanel mission={mission} decision={decision} currentDecisionId={null} onJumpToCurrent={() => undefined} />);
    try {
      await flushAsync();
      expect(handle.container.querySelector("[data-testid=decision-option]")).toBeNull();
      const send = handle.container.querySelector<HTMLButtonElement>("[data-testid=decision-text-send]")!;
      const textarea = handle.container.querySelector<HTMLTextAreaElement>("[data-testid=decision-text]")!;
      expect(send.disabled).toBe(true);
      pressKey(textarea, "Enter", { ctrlKey: true });
      await flushAsync();
      expect(answer).not.toHaveBeenCalled();
      setValue(textarea, "staging");
      expect(send.disabled).toBe(false);
      pressKey(textarea, "Enter", { ctrlKey: true, isComposing: true });
      dispatch(textarea, new CompositionEvent("compositionstart", { bubbles: true }));
      pressKey(textarea, "Enter", { ctrlKey: true });
      pressKey(textarea, "Enter");
      await flushAsync();
      expect(answer).not.toHaveBeenCalled();
      dispatch(textarea, new CompositionEvent("compositionend", { bubbles: true }));
      pressKey(textarea, "Enter", { ctrlKey: true });
      await flushAsync();
      expect(answer).toHaveBeenCalledTimes(1);
      expect(answer.mock.calls[0][0]).toMatchObject({ decision_id: decision.id, option_id: null, answer_ref: ref });
    } finally {
      handle.unmount();
    }
  });

  it("에이전트 막힘 결정은 보고 본문을 보여 주고 지시를 붙여 다시 시도한다(원문 JSON 노출 없음)", async () => {
    const client = installMockClient();
    const report = await uploadMockText(client, "배포 키가 없어 진행할 수 없습니다.\n설정에서 키를 등록해 주세요.");
    const mission = fakeMission();
    const task = fakeTask(mission.id, "blocked", { title: "배포 준비", blocked_code: "provider_blocked:needs_credentials", attempt_count: 1 });
    const run = fakeRun(mission.id, task, "succeeded");
    task.active_run_id = null;
    const question = await uploadMockText(client, JSON.stringify({ kind: "provider_blocked", version: 1, task_id: task.id, task_title: task.title,
      run_id: run.id, code: "needs_credentials", report_ref: report, attempt_count: 1, max_attempts_per_task: 3, message: "Agent blocked: needs_credentials" }));
    const decision = fakeDecision(mission.id, { kind: "recovery", question_ref: question, requesting_run_id: run.id, affected_task_ids: [task.id],
      options: [{ id: "retry_with_instruction", label: "Retry" }, { id: "change_model", label: "Change" }, { id: "replan", label: "Replan" }, { id: "stop_mission", label: "Stop" }] });
    seedStore({ missions: [mission], tasks: [task], runs: [run], decisions: [decision] });
    const { ref } = stubUpload(client);
    const answer = vi.spyOn(client, "missionDecisionAnswer").mockResolvedValue(mutationResult(mission));
    const handle = renderUi(<DecisionPanel mission={mission} decision={decision} currentDecisionId={null} onJumpToCurrent={() => undefined} />);
    try {
      await flushAsync();
      expect(handle.container.querySelector("[data-testid=decision-question]")?.textContent)
        .toBe(t("missions.decision.situation.providerBlocked", { task: "배포 준비", attempt: 1, limit: 3 }));
      expect(handle.container.querySelector("[data-testid=decision-blocked-report]")?.textContent).toContain("배포 키가 없어 진행할 수 없습니다.");
      expect(handle.container.textContent).not.toContain("\"kind\"");
      expect(handle.container.querySelector("[data-testid=decision-details]")?.textContent).toContain(t("missions.decision.blocked.code", { code: "needs_credentials" }));
      expect([...handle.container.querySelectorAll("[data-testid=decision-option]")].map((b) => b.textContent)).toEqual([
        t("missions.decision.option.retryWithInstruction"), t("missions.decision.option.changeModel"),
        t("missions.decision.option.replan"), t("missions.decision.option.stopMission"),
      ]);
      const retry = handle.container.querySelector<HTMLButtonElement>("[data-option-id=retry_with_instruction]")!;
      expect(retry.disabled).toBe(false);
      setValue(handle.container.querySelector<HTMLTextAreaElement>("[data-testid=decision-text]")!, "키는 staging 것을 쓰세요");
      click(retry);
      await flushAsync();
      expect(answer.mock.calls[0][0]).toMatchObject({ decision_id: decision.id, option_id: "retry_with_instruction", answer_ref: ref });
    } finally {
      handle.unmount();
    }
  });

  it("에이전트 막힘의 모델 변경은 재배정이 성공한 뒤에 change_model로 답한다", async () => {
    const client = installMockClient();
    const binding = { ...compatibleBinding(), label: "Other", model_id: "other-model" };
    const mission = fakeMission();
    mission.policy.allowed_binding_ids = [binding.id];
    const task = fakeTask(mission.id, "blocked", { blocked_code: "provider_blocked:unsupported_tool" });
    const run = fakeRun(mission.id, task, "succeeded");
    task.active_run_id = null;
    const decision = fakeDecision(mission.id, { kind: "recovery", requesting_run_id: run.id, affected_task_ids: [task.id],
      options: [{ id: "change_model", label: "Change" }, { id: "stop_mission", label: "Stop" }] });
    seedStore({ missions: [mission], tasks: [task], runs: [run], decisions: [decision] });
    client.bindingList = vi.fn(async () => ({ bindings: [binding] }));
    const order: string[] = [];
    const control = vi.spyOn(client, "missionTaskControl").mockImplementation(async () => { order.push("reassign"); return mutationResult(mission); });
    const answer = vi.spyOn(client, "missionDecisionAnswer").mockImplementation(async () => { order.push("answer"); return mutationResult(mission); });
    const handle = renderUi(<DecisionPanel mission={mission} decision={decision} currentDecisionId={null} onJumpToCurrent={() => undefined} />);
    try {
      click(handle.container.querySelector("[data-option-id=change_model]")!);
      await flushAsync();
      expect(answer).not.toHaveBeenCalled();
      const pick = [...handle.container.querySelectorAll<HTMLButtonElement>("[data-testid=reassign-picker] button")].find((b) => b.textContent?.includes("Other"))!;
      expect(pick.title).toBe(t("missions.detail.reassignOnlyTitle", { model: "other-model" }));
      click(pick);
      await flushAsync();
      expect(order).toEqual(["reassign", "answer"]);
      expect(control.mock.calls[0][0]).toMatchObject({ task_id: task.id, action: "reassign", binding_id: binding.id });
      expect(answer.mock.calls[0][0]).toMatchObject({ decision_id: decision.id, option_id: "change_model", answer_ref: null });
    } finally {
      handle.unmount();
    }
  });
});

describe("DecisionPanel 실패·한도 결정", () => {
  it("실패 결정은 실행 결과의 첫 줄과 로그인 안내·설정 열기를 보여 준다", async () => {
    const client = installMockClient();
    const result = await uploadMockText(client, "\n401 Unauthorized: session expired\n    at provider.call\n");
    const mission = fakeMission();
    const task = fakeTask(mission.id, "failed", { title: "배포 준비" });
    const run = fakeRun(mission.id, task, "failed", { failure_code: "AUTH_REQUIRED", result_ref: result });
    const decision = fakeDecision(mission.id, { kind: "recovery", affected_task_ids: [task.id], requesting_run_id: run.id,
      options: [{ id: "retry_failed_task", label: "Host retry" }, { id: "stop_failed_mission", label: "Host stop" }] });
    seedStore({ missions: [mission], tasks: [task], runs: [run], decisions: [decision] });
    const handle = renderUi(<DecisionPanel mission={mission} decision={decision} currentDecisionId={null} onJumpToCurrent={() => undefined} />);
    try {
      await flushAsync();
      expect(handle.container.querySelector("[data-testid=decision-failure-message]")?.textContent)
        .toBe(t("missions.decision.failure.message", { line: "401 Unauthorized: session expired" }));
      expect(handle.container.textContent).not.toContain("provider.call");
      expect(handle.container.querySelector("[data-testid=decision-change-model]")).toBeNull();
      expect(handle.container.querySelector("[data-testid=decision-login-text]")).toBeNull();
      click(handle.container.querySelector("[data-testid=decision-login-help]")!);
      expect(handle.container.querySelector("[data-testid=decision-login-text]")?.textContent).toContain("codex login");
      click(handle.container.querySelector("[data-testid=decision-open-settings]")!);
      expect(useWorkbenchStore.getState()).toMatchObject({ page: "settings", settingsGroup: "missions" });
    } finally {
      handle.unmount();
      useWorkbenchStore.setState({ page: "terminal", settingsGroup: null });
    }
  });

  it.each(["MODEL_UNAVAILABLE", "CAPABILITY_UNSUPPORTED", "PROVIDER_RATE_LIMITED"] as const)("%s 실패 결정 안에서 모델을 바꿔 바로 다시 시도한다", async (code) => {
    const client = installMockClient();
    const binding = { ...compatibleBinding(), label: "Fallback", model_id: "fallback-model" };
    const mission = fakeMission();
    mission.policy.allowed_binding_ids = [binding.id];
    const task = fakeTask(mission.id, "failed");
    const reset = Date.UTC(2026, 8, 17, 9, 0);
    const run = fakeRun(mission.id, task, "failed", { failure_code: code,
      rate_limit: code === "PROVIDER_RATE_LIMITED" ? { observed_at_unix_ms: "1", resets_at_unix_ms: String(reset) } : null });
    task.active_run_id = null;
    const decision = fakeDecision(mission.id, { kind: "recovery", affected_task_ids: [task.id], requesting_run_id: run.id,
      options: [{ id: "retry_failed_task", label: "Host retry" }, { id: "stop_failed_mission", label: "Host stop" }] });
    seedStore({ missions: [mission], tasks: [task], runs: [run], decisions: [decision] });
    client.bindingList = vi.fn(async () => ({ bindings: [binding] }));
    const control = vi.spyOn(client, "missionTaskControl").mockResolvedValue(mutationResult(mission));
    const handle = renderUi(<DecisionPanel mission={mission} decision={decision} currentDecisionId={null} onJumpToCurrent={() => undefined} />);
    try {
      await flushAsync();
      const resetLine = handle.container.querySelector("[data-testid=decision-rate-limit-reset]");
      if (code === "PROVIDER_RATE_LIMITED") expect(resetLine?.textContent).toContain(new Date(reset).toLocaleString("ko"));
      else expect(resetLine).toBeNull();
      click(handle.container.querySelector("[data-testid=decision-change-model]")!);
      await flushAsync();
      const pick = [...handle.container.querySelectorAll<HTMLButtonElement>("[data-testid=reassign-picker] button")].find((b) => b.textContent?.includes("Fallback"))!;
      expect(pick.title).toBe(t("missions.detail.reassignRetryTitle", { model: binding.model_id }));
      click(pick);
      await flushAsync();
      expect(control).toHaveBeenCalledTimes(1);
      expect(control.mock.calls[0][0]).toMatchObject({ task_id: task.id, action: "retry", binding_id: binding.id, expected_revision: mission.revision });
      expect(handle.container.querySelector("[data-testid=reassign-picker]")).toBeNull();
    } finally {
      handle.unmount();
    }
  });

  it("활성 시간 한도 결정은 한도 편집기를 열어 바꾼 한도만 최신 정책 위에 저장한다", async () => {
    const client = installMockClient();
    const question = await uploadMockText(client, "The mission execution budget (active_time_limit) is exhausted.");
    const mission = fakeMission({ active_time_ms: "3600000" });
    mission.policy.active_time_limit_ms = "3600000";
    const task = fakeTask(mission.id, "blocked", { blocked_code: "active_time_limit" });
    const decision = fakeDecision(mission.id, { kind: "budget", blocking: true, question_ref: question, affected_task_ids: [task.id],
      options: [{ id: "stop_mission", label: "Stop mission" }] });
    seedStore({ missions: [mission], tasks: [task], decisions: [decision] });
    const update = vi.spyOn(client, "missionPolicyUpdate").mockResolvedValue(mutationResult(mission));
    const answer = vi.spyOn(client, "missionDecisionAnswer").mockResolvedValue(mutationResult(mission));
    const handle = renderUi(<DecisionPanel mission={mission} decision={decision} currentDecisionId={null} onJumpToCurrent={() => undefined} />);
    try {
      await flushAsync();
      expect(handle.container.querySelector("[data-testid=decision-question]")?.textContent).toBe(t("missions.decision.situation.activeTimeLimit"));
      expect(handle.container.querySelector("[data-testid=decision-original]")?.textContent).toContain("(active_time_limit)");
      expect(handle.container.querySelector("[data-testid=policy-limits-editor]")).toBeNull();
      click(handle.container.querySelector("[data-testid=decision-adjust-limits]")!);
      const editor = handle.container.querySelector("[data-testid=policy-limits-editor]")!;
      expect(editor.textContent).toContain(t("missions.limits.usage", { used: "1:00:00", limit: "1:00:00" }));
      setValue(editor.querySelector<HTMLSelectElement>("[data-testid=limits-active-unit]")!, "hours");
      setValue(editor.querySelector<HTMLInputElement>("[data-testid=limits-active]")!, "5");
      act(() => useMissionStore.setState((s) => ({ missions: { ...s.missions,
        [mission.id]: { ...mission, revision: "7", policy: { ...mission.policy, max_parallel_runs: 3 } } } })));
      click(editor.querySelector("[data-testid=limits-save]")!);
      await flushAsync();
      expect(update).toHaveBeenCalledTimes(1);
      expect(update.mock.calls[0][0]).toMatchObject({ expected_revision: "7",
        policy: { active_time_limit_ms: "18000000", max_parallel_runs: 3 } });
      expect(answer).not.toHaveBeenCalled();
    } finally {
      handle.unmount();
    }
  });

  it("adjust_limits 선택지는 답하지 않고 편집기만 연다(한도 저장이 결정을 푼다)", async () => {
    const client = installMockClient();
    const mission = fakeMission();
    const task = fakeTask(mission.id, "blocked", { blocked_code: "automatic_start_limit" });
    const decision = fakeDecision(mission.id, { kind: "budget", affected_task_ids: [task.id],
      options: [{ id: "adjust_limits", label: "Adjust" }, { id: "stop_mission", label: "Stop" }] });
    seedStore({ missions: [mission], tasks: [task], decisions: [decision] });
    const update = vi.spyOn(client, "missionPolicyUpdate").mockResolvedValue(mutationResult(mission));
    const answer = vi.spyOn(client, "missionDecisionAnswer").mockResolvedValue(mutationResult(mission));
    const handle = renderUi(<DecisionPanel mission={mission} decision={decision} currentDecisionId={null} onJumpToCurrent={() => undefined} />);
    try {
      expect(handle.container.querySelector("[data-testid=decision-adjust-limits]")).toBeNull();
      click(handle.container.querySelector("[data-option-id=adjust_limits]")!);
      await flushAsync();
      expect(answer).not.toHaveBeenCalled();
      const editor = handle.container.querySelector("[data-testid=policy-limits-editor]")!;
      setValue(editor.querySelector<HTMLInputElement>("[data-testid=limits-starts]")!, "9999");
      click(editor.querySelector("[data-testid=limits-save]")!);
      await flushAsync();
      expect(update.mock.calls[0][0].policy.max_automatic_starts).toBe(512);
      expect(handle.container.querySelector("[data-testid=limits-clamped]")).not.toBeNull();
      // 계약: adjust_limits 답변은 policy_update_required로 거절된다 — 보내지 않는다.
      expect(answer).not.toHaveBeenCalled();
    } finally {
      handle.unmount();
    }
  });
});

describe("DecisionPanel 전송 오류", () => {
  it("REVISION_CONFLICT면 재동기화 후 최신 revision과 새 request_id로 한 번 더 답한다", async () => {
    const client = installMockClient();
    const mission = fakeMission({ revision: "5" });
    const decision = fakeDecision(mission.id);
    seedStore({ missions: [mission], decisions: [decision] });
    const { sync, restore } = stubSync(mission, "8");
    const answer = vi.spyOn(client, "missionDecisionAnswer")
      .mockRejectedValueOnce(new RpcClientError("REVISION_CONFLICT", "moved", false, { current_revision: "8" }))
      .mockResolvedValueOnce(mutationResult(mission));
    const handle = renderUi(<DecisionPanel mission={mission} decision={decision} currentDecisionId={null} onJumpToCurrent={() => undefined} />);
    try {
      click(handle.container.querySelector("[data-testid=decision-option]")!);
      await flushAsync();
      expect(sync).toHaveBeenCalledTimes(1);
      expect(answer.mock.calls.map((call) => call[0].expected_revision)).toEqual(["5", "8"]);
      expect(answer.mock.calls[0][0].request_id).not.toBe(answer.mock.calls[1][0].request_id);
      expect(handle.container.querySelector("[role=alert]")).toBeNull();
    } finally {
      handle.unmount();
      restore();
    }
  });

  it("답변 오류는 원인 문장·행동 버튼·자세히(원문)를 한 번만 알린다", async () => {
    const client = installMockClient();
    const mission = fakeMission();
    const decision = fakeDecision(mission.id);
    seedStore({ missions: [mission], decisions: [decision] });
    const { sync, restore } = stubSync(mission, "6");
    vi.spyOn(client, "missionDecisionAnswer").mockRejectedValue(new RpcClientError("STALE_DECISION", "decision moved on"));
    const handle = renderUi(<DecisionPanel mission={mission} decision={decision} currentDecisionId={null} onJumpToCurrent={() => undefined} />);
    try {
      click(handle.container.querySelector("[data-testid=decision-option]")!);
      await flushAsync();
      const alerts = handle.container.querySelectorAll("[role=alert]");
      expect(alerts).toHaveLength(1);
      const message = alerts[0].querySelector("[data-testid=mission-error-message]")!.textContent;
      expect(message).toBe(t("missions.error.code.staleDecision"));
      expect(message).not.toContain("STALE_DECISION");
      expect(alerts[0].querySelector("details")?.textContent).toContain("STALE_DECISION: decision moved on");
      const action = alerts[0].querySelector("[data-testid=mission-error-action]")!;
      expect(action.textContent).toBe(t("missions.error.action.resync"));
      click(action);
      expect(sync).toHaveBeenCalledWith(mission.id);
    } finally {
      handle.unmount();
      restore();
    }
  });
});
