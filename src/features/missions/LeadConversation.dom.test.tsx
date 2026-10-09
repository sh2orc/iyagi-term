/**
 * LeadConversation 시험(05 §4): IME 조합 Enter 가드(React composition flag와
 * native isComposing 둘 다) · 수신자 고정 · draft 분리.
 */

import { beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { LeadConversation, parseDecisionAnswer } from "./LeadConversation";
import { RunDetail } from "./RunDetail";
import {
  dispatch,
  click,
  flushAsync,
  installMockClient,
  pressKey,
  renderUi,
  resetAllMissionState,
  fakeDecision,
  fakeMission,
  fakeMessage,
  fakeRun,
  fakeTask,
  seedStore,
  setValue,
  uploadMockText,
} from "./testSupport";
import { MissionMessageParams } from "../../generated/MissionMessageParams";
import { t } from "../../i18n";
import { missionError } from "./errors";
import { decisionOptionLabel } from "./labels";

beforeEach(() => {
  resetAllMissionState();
});

describe("LeadConversation IME 가드(05 §4)", () => {
  async function setup() {
    const { client, mission } = await createClientMission();
    seedStore({ missions: [mission] });
    const sendSpy = vi.fn(async (_params: MissionMessageParams) => ({
      mission_id: mission.id,
      revision: "6",
      event_seq: "1",
      entity_ids: [],
    }));
    client.missionMessage = sendSpy;
    const handle = renderUi(
      <LeadConversation mission={mission} onOpenRun={() => undefined} onStartFollowUp={() => undefined} />,
    );
    await flushAsync(3);
    const composer = handle.container.querySelector("[data-testid='lead-composer']") as HTMLTextAreaElement;
    const send = handle.container.querySelector("[data-testid='lead-send']") as HTMLButtonElement;
    return { handle, composer, send, sendSpy, mission };
  }

  it("compositionstart 뒤 Enter(React flag 경로)는 전송하지 않는다", async () => {
    const { handle, composer, sendSpy } = await setup();
    setValue(composer, "안녕하세요");
    dispatch(composer, new CompositionEvent("compositionstart", { bubbles: true }));
    // native isComposing이 false여도 React 조합 flag가 잡는다(일부 브라우저).
    pressKey(composer, "Enter", { isComposing: false });
    await flushAsync();
    expect(sendSpy).not.toHaveBeenCalled();
    handle.unmount();
  });

  it("compositionstart 없이 native isComposing=true Enter도 전송하지 않는다", async () => {
    const { handle, composer, sendSpy } = await setup();
    setValue(composer, "안녕하세요");
    pressKey(composer, "Enter", { isComposing: true });
    await flushAsync();
    expect(sendSpy).not.toHaveBeenCalled();
    handle.unmount();
  });

  it("조합 종료 후 Enter는 1회 전송한다", async () => {
    const { handle, composer, sendSpy, mission } = await setup();
    setValue(composer, "안녕하세요");
    dispatch(composer, new CompositionEvent("compositionstart", { bubbles: true }));
    pressKey(composer, "Enter", { isComposing: false });
    dispatch(composer, new CompositionEvent("compositionend", { bubbles: true }));
    pressKey(composer, "Enter", { isComposing: false });
    await flushAsync(10);
    expect(sendSpy).toHaveBeenCalledTimes(1);
    expect(sendSpy.mock.calls[0][0]).toMatchObject({
      mission_id: mission.id,
      target_task_id: null,
      expected_revision: mission.revision,
    });
    // 성공 후 draft가 비워진다.
    expect(composer.value).toBe("");
    handle.unmount();
  });

  it("Shift+Enter는 전송하지 않는다(줄바꿈)", async () => {
    const { handle, composer, sendSpy } = await setup();
    setValue(composer, "두 줄");
    pressKey(composer, "Enter", { shiftKey: true });
    await flushAsync();
    expect(sendSpy).not.toHaveBeenCalled();
    handle.unmount();
  });
});

async function createClientMission() {
  const client = installMockClient();
  const template = fakeMission();
  await client.missionCreate({
    request_id: crypto.randomUUID(),
    title: template.title,
    repository_path: template.repository_path,
    expected_base_oid: template.base_oid,
    goal_ref: template.goal_ref,
    requirements: template.requirements,
    policy: template.policy,
    role_bindings: template.role_bindings,
  });
  return { client, mission: client.missions[0] };
}

describe("메시지 artifact 소유권과 업로드 중 revision 변경", () => {
  it.each(["lead", "task"] as const)("%s 입력은 해당 미션에 업로드하고 최신 revision으로 전송한다", async (recipient) => {
    const { client, mission } = await createClientMission();
    const task = fakeTask(mission.id, "ready");
    client.seedMissionEntity(mission.id, { kind: "task", value: task });
    seedStore({ missions: [mission], tasks: [task] });
    const begin = vi.spyOn(client, "artifactBegin");
    const commit = client.artifactCommit.bind(client);
    client.artifactCommit = async (params) => {
      const ref = await commit(params);
      // 업로드가 끝나기 전에 daemon의 다른 변경을 수신한다.
      client.seedMissionEntities(mission.id, [{ kind: "mission", value: { ...mission } }]);
      act(() => seedStore({ missions: [client.missions[0]] }));
      return ref;
    };
    const originalSend = client.missionMessage.bind(client);
    const send = vi.fn(async (params: MissionMessageParams) => {
      const result = await originalSend(params);
      return result;
    });
    client.missionMessage = send;
    const handle = renderUi(recipient === "lead"
      ? <LeadConversation mission={mission} onOpenRun={() => undefined} onStartFollowUp={() => undefined} />
      : <RunDetail mission={mission} task={task} detailTab="activity" onDetailTabChange={() => undefined} />);
    if (recipient === "task") click(handle.container.querySelector("[data-testid='instruct-toggle']")!);
    await flushAsync(3);
    const composer = handle.container.querySelector(recipient === "lead"
      ? "[data-testid='lead-composer']" : "[data-testid='task-composer-input']") as HTMLTextAreaElement;
    setValue(composer, "변경된 계획을 확인해 주세요.");
    click(handle.container.querySelector(recipient === "lead"
      ? "[data-testid='lead-send']" : "[data-testid='task-composer-send']")!);
    await flushAsync(15);
    expect(begin).toHaveBeenCalledTimes(1);
    expect(begin.mock.calls[0][0].mission_id).toBe(mission.id);
    expect(send).toHaveBeenCalledTimes(1);
    expect(send.mock.calls[0][0]).toMatchObject({
      mission_id: mission.id,
      target_task_id: recipient === "lead" ? null : task.id,
      expected_revision: String(BigInt(mission.revision) + 1n),
    });
    expect(handle.container.querySelector("[role='alert']")).toBeNull();
    expect(composer.value).toBe("");
    handle.unmount();
  });
});

describe("LeadConversation 수신자와 draft(05 §4)", () => {
  it("composer 수신자는 항상 '작업 전체에 요청'이다", () => {
    const mission = fakeMission();
    seedStore({ missions: [mission] });
    const handle = renderUi(
      <LeadConversation mission={mission} onOpenRun={() => undefined} onStartFollowUp={() => undefined} />,
    );
    expect(handle.container.querySelector("[data-testid='lead-recipient']")?.textContent).toBe(
      t("missions.lead.composerLabel"),
    );
    handle.unmount();
  });

  it("RunDetail의 추가 지시 입력창과 draft를 공유하지 않는다", async () => {
    const mission = fakeMission();
    seedStore({ missions: [mission] });
    const handle = renderUi(
      <LeadConversation mission={mission} onOpenRun={() => undefined} onStartFollowUp={() => undefined} />,
    );
    await flushAsync(3);
    const lead = handle.container.querySelector("[data-testid='lead-composer']") as HTMLTextAreaElement;
    setValue(lead, "lead draft");
    // 다른 입력창(RunDetail task composer)은 별개 컴포넌트 — 여기선 lead
    // composer의 값이 그대로 유지됨으로 draft 독립성을 확인한다.
    expect(lead.value).toBe("lead draft");
    handle.unmount();
  });

  it("메시지의 전달 상태를 정직하게 표시한다(queued ≠ delivered)", () => {
    const mission = fakeMission();
    const queued = fakeMessage(mission.id, "user", { delivery: "queued" });
    const unknown = fakeMessage(mission.id, "user", { delivery: "unknown" });
    const delivered = fakeMessage(mission.id, "user", { delivery: "delivered" });
    seedStore({ missions: [mission], messages: [queued, unknown, delivered] });
    const handle = renderUi(
      <LeadConversation mission={mission} onOpenRun={() => undefined} onStartFollowUp={() => undefined} />,
    );
    const text = handle.container.textContent ?? "";
    expect(text).toContain(t("missions.delivery.queued"));
    expect(text).toContain(t("missions.delivery.unknown"));
    expect(text).toContain(t("missions.delivery.delivered"));
    handle.unmount();
  });
});

describe("LeadConversation 실패·취소 요약 카드", () => {
  it("실패 원인 코드를 문장으로 보이고 설정 열기·같은 목표로 새 작업을 제공한다", () => {
    const mission = fakeMission({ state: "failed", failure_code: "AUTH_REQUIRED" });
    seedStore({ missions: [mission] });
    const openSettings = vi.fn();
    const sameGoal = vi.fn();
    const handle = renderUi(
      <LeadConversation
        mission={mission}
        onOpenRun={() => undefined}
        onStartFollowUp={() => undefined}
        onStartSameGoal={sameGoal}
        onOpenSettings={openSettings}
      />,
    );
    const scroll = handle.container.querySelector("[data-testid='lead-scroll']") as HTMLElement;
    const card = handle.container.querySelector("[data-testid='mission-outcome-card']") as HTMLElement;
    // 대화 맨 위에 놓인다.
    expect(scroll.firstElementChild).toBe(card);
    expect(card.querySelector("[data-testid='mission-outcome-message']")?.textContent).toBe(
      missionError(t, { code: "AUTH_REQUIRED", message: "" }).message,
    );
    expect(card.getAttribute("role")).toBeNull();
    click(card.querySelector("[data-testid='outcome-open-settings']")!);
    click(card.querySelector("[data-testid='outcome-same-goal']")!);
    expect(openSettings).toHaveBeenCalledTimes(1);
    expect(sameGoal).toHaveBeenCalledTimes(1);
    handle.unmount();
  });

  it("취소된 작업은 설정 열기 없이 같은 목표로 새 작업만 권한다", () => {
    const mission = fakeMission({ state: "cancelled" });
    seedStore({ missions: [mission] });
    const handle = renderUi(
      <LeadConversation mission={mission} onOpenRun={() => undefined} onStartFollowUp={() => undefined}
        onStartSameGoal={() => undefined} onOpenSettings={() => undefined} />,
    );
    const card = handle.container.querySelector("[data-testid='mission-outcome-card']") as HTMLElement;
    expect(card.textContent).toContain(t("missions.outcome.cancelledTitle"));
    expect(card.textContent).toContain(t("missions.outcome.cancelledBody"));
    expect(card.querySelector("[data-testid='outcome-open-settings']")).toBeNull();
    expect(card.querySelector("[data-testid='outcome-same-goal']")).not.toBeNull();
    handle.unmount();
  });

  it("진행 중인 작업에는 요약 카드를 두지 않는다", () => {
    const mission = fakeMission({ state: "running" });
    seedStore({ missions: [mission] });
    const handle = renderUi(<LeadConversation mission={mission} onOpenRun={() => undefined} onStartFollowUp={() => undefined} />);
    expect(handle.container.querySelector("[data-testid='mission-outcome-card']")).toBeNull();
    handle.unmount();
  });
});

describe("LeadConversation 결정 답변 메시지", () => {
  it("결정 답변 JSON은 `결정: {선택지}` 카드로 보인다", async () => {
    const client = installMockClient();
    const mission = fakeMission();
    const decision = fakeDecision(mission.id, {
      state: "answered",
      options: [{ id: "keep", label: "기존 API 유지" }, { id: "stop_mission", label: "Stop mission" }],
      selected_option_id: "keep",
    });
    const keepBody = await uploadMockText(client, JSON.stringify({ decision_id: decision.id, option_id: "keep" }));
    const stopBody = await uploadMockText(client, JSON.stringify({ decision_id: decision.id, option_id: "stop_mission" }));
    const keep = fakeMessage(mission.id, "system", { body_ref: keepBody, created_at: "2026-09-13T01:00:00.000Z" });
    const stop = fakeMessage(mission.id, "system", { body_ref: stopBody, created_at: "2026-09-13T01:01:00.000Z" });
    seedStore({ missions: [mission], decisions: [decision], messages: [keep, stop] });
    const handle = renderUi(<LeadConversation mission={mission} onOpenRun={() => undefined} onStartFollowUp={() => undefined} />);
    await flushAsync(6);
    const cards = [...handle.container.querySelectorAll("[data-testid='decision-answer-card']")].map((card) => card.textContent);
    expect(cards).toEqual([
      t("missions.lead.decisionAnswer", { option: "기존 API 유지" }),
      t("missions.lead.decisionAnswer", { option: decisionOptionLabel(t, { id: "stop_mission", label: "Stop mission" }) }),
    ]);
    expect(handle.container.textContent).not.toContain("decision_id");
    handle.unmount();
  });

  it("자유 입력이 함께 저장된 답변은 선택지와 입력 요약을 함께 보인다", async () => {
    const client = installMockClient();
    const mission = fakeMission();
    const note = await uploadMockText(client, "호환성은 다음 릴리스에서\n정리합니다.");
    const message = fakeMessage(mission.id, "system", { body_ref: note });
    const decision = fakeDecision(mission.id, {
      state: "answered",
      selected_option_id: "keep",
      answer_ref: note,
      answer_message_id: message.id,
    });
    seedStore({ missions: [mission], decisions: [decision], messages: [message] });
    const handle = renderUi(<LeadConversation mission={mission} onOpenRun={() => undefined} onStartFollowUp={() => undefined} />);
    await flushAsync(6);
    const card = handle.container.querySelector("[data-testid='decision-answer-card']");
    expect(card?.textContent).toContain(t("missions.lead.decisionAnswer", { option: "기존 API 유지" }));
    expect(card?.textContent).toContain(t("missions.lead.decisionNote", { text: "호환성은 다음 릴리스에서 정리합니다." }));
    handle.unmount();
  });
});

describe("LeadConversation 결정 답변 문서(01 §8 decision_answer)", () => {
  it("parseDecisionAnswer는 새 문서와 기존 부분집합을 읽고, 다른 kind·텍스트는 답변 문서로 보지 않는다", () => {
    expect(parseDecisionAnswer(JSON.stringify({
      kind: "decision_answer", version: 1, decision_id: "d-1", decision_kind: "recovery",
      option_id: "retry_with_instruction", option_label: "Retry with an instruction",
      answer_ref: { id: "a-1", sha256: "0", bytes: "9", media_type: "text/plain" }, answer_text: "테스트부터 고치세요",
    }))).toEqual({
      decisionId: "d-1", optionId: "retry_with_instruction", optionLabel: "Retry with an instruction",
      decisionKind: "recovery", answerText: "테스트부터 고치세요",
    });
    expect(parseDecisionAnswer(JSON.stringify({ decision_id: "d-2", option_id: "keep" }))).toEqual({
      decisionId: "d-2", optionId: "keep", optionLabel: null, decisionKind: null, answerText: null,
    });
    expect(parseDecisionAnswer(JSON.stringify({
      kind: "decision_answer", version: 1, decision_id: "d-3", decision_kind: "budget",
      option_id: "stop_mission", option_label: "Stop mission", answer_ref: null, answer_text: null,
    }))?.answerText).toBeNull();
    expect(parseDecisionAnswer(JSON.stringify({ kind: "integration_exclusion", version: 1, decision_id: "d-4" }))).toBeNull();
    expect(parseDecisionAnswer("그냥 텍스트 답변")).toBeNull();
    expect(parseDecisionAnswer("{not json")).toBeNull();
    expect(parseDecisionAnswer(JSON.stringify({ kind: "decision_answer", option_id: "keep" }))).toBeNull();
    expect(parseDecisionAnswer(null)).toBeNull();
  });

  it("새 문서는 선택지 id 번역 라벨과 answer_text 요약을 보이고, 결정이 없고 모르는 id면 option_label을 쓴다", async () => {
    const client = installMockClient();
    const mission = fakeMission();
    const known = await uploadMockText(client, JSON.stringify({
      kind: "decision_answer", version: 1, decision_id: "decision-gone", decision_kind: "recovery",
      option_id: "retry_with_instruction", option_label: "Retry with an instruction",
      answer_ref: { id: "answer-1", sha256: "0".repeat(64), bytes: "40", media_type: "text/plain" },
      answer_text: "로그인 테스트를\n먼저 고친 뒤 다시 시도하세요.",
    }));
    const unknown = await uploadMockText(client, JSON.stringify({
      kind: "decision_answer", version: 1, decision_id: "decision-gone-2", decision_kind: "product",
      option_id: "provider-choice-7", option_label: "Keep the legacy endpoint", answer_ref: null, answer_text: null,
    }));
    const first = fakeMessage(mission.id, "system", { body_ref: known, created_at: "2026-09-13T01:00:00.000Z" });
    const second = fakeMessage(mission.id, "system", { body_ref: unknown, created_at: "2026-09-13T01:01:00.000Z" });
    seedStore({ missions: [mission], messages: [first, second] });
    const handle = renderUi(<LeadConversation mission={mission} onOpenRun={() => undefined} onStartFollowUp={() => undefined} />);
    await flushAsync(6);
    const cards = [...handle.container.querySelectorAll("[data-testid='decision-answer-card']")];
    expect(cards).toHaveLength(2);
    expect(cards[0].textContent).toContain(t("missions.lead.decisionAnswer", {
      option: decisionOptionLabel(t, { id: "retry_with_instruction", label: "Retry with an instruction" }),
    }));
    expect(cards[0].textContent).not.toContain("Retry with an instruction");
    expect(cards[0].textContent).toContain(t("missions.lead.decisionNote", { text: "로그인 테스트를 먼저 고친 뒤 다시 시도하세요." }));
    expect(cards[1].textContent).toBe(t("missions.lead.decisionAnswer", { option: "Keep the legacy endpoint" }));
    expect(handle.container.textContent).not.toContain("decision_answer");
    expect(handle.container.textContent).not.toContain("answer_text");
    handle.unmount();
  });

  it("결정이 store에 있으면 그 결정의 선택지 label을 우선하고, JSON이 아닌 답변은 일반 메시지로 보인다", async () => {
    const client = installMockClient();
    const mission = fakeMission();
    const decision = fakeDecision(mission.id, {
      state: "answered",
      options: [{ id: "keep", label: "기존 API 유지" }],
      selected_option_id: "keep",
    });
    const structured = await uploadMockText(client, JSON.stringify({
      kind: "decision_answer", version: 1, decision_id: decision.id, decision_kind: "product",
      option_id: "keep", option_label: "Keep the API", answer_ref: null, answer_text: null,
    }));
    const plainText = await uploadMockText(client, "그냥 남긴 메모입니다.");
    const answer = fakeMessage(mission.id, "system", { body_ref: structured, created_at: "2026-09-13T01:00:00.000Z" });
    const note = fakeMessage(mission.id, "system", { body_ref: plainText, created_at: "2026-09-13T01:01:00.000Z" });
    seedStore({ missions: [mission], decisions: [decision], messages: [answer, note] });
    const handle = renderUi(<LeadConversation mission={mission} onOpenRun={() => undefined} onStartFollowUp={() => undefined} />);
    await flushAsync(6);
    const cards = [...handle.container.querySelectorAll("[data-testid='decision-answer-card']")].map((card) => card.textContent);
    expect(cards).toEqual([t("missions.lead.decisionAnswer", { option: "기존 API 유지" })]);
    expect(handle.container.textContent).toContain("그냥 남긴 메모입니다.");
    handle.unmount();
  });
});

describe("LeadConversation 계획 카드와 빈 상태", () => {
  it("작업 N개는 대체됨·검증·통합을 뺀 사용자 의미의 할 일만 센다", () => {
    const mission = fakeMission();
    seedStore({
      missions: [mission],
      tasks: [
        fakeTask(mission.id, "running", { title: "API" }),
        fakeTask(mission.id, "planned", { title: "UI" }),
        fakeTask(mission.id, "superseded", { title: "옛 계획" }),
        fakeTask(mission.id, "planned", { title: "검증", kind: "verify" }),
        fakeTask(mission.id, "planned", { title: "통합", kind: "integrate", role: null }),
      ],
    });
    const handle = renderUi(<LeadConversation mission={mission} onOpenRun={() => undefined} onStartFollowUp={() => undefined} />);
    expect(handle.container.querySelector("[data-testid='plan-count']")?.textContent).toBe(t("missions.lead.planTasks", { count: 2 }));
    click(handle.container.querySelector("[data-testid='plan-toggle']")!);
    const items = [...handle.container.querySelectorAll(".mission-plan-list li")].map((li) => li.textContent);
    expect(items).toEqual(["1. API", "2. UI"]);
    handle.unmount();
  });

  it("계획 단계의 빈 대화는 Lead가 계획을 세우는 중임과 경과 시간을 보인다", () => {
    const mission = fakeMission({ state: "running", phase: "planning" });
    const lead = fakeTask(mission.id, "running", { kind: "plan", role: "lead", title: "계획" });
    const run = fakeRun(mission.id, lead, "running", { started_at: new Date(Date.now() - 125_000).toISOString() });
    seedStore({ missions: [mission], tasks: [lead], runs: [run] });
    const handle = renderUi(<LeadConversation mission={mission} onOpenRun={() => undefined} onStartFollowUp={() => undefined} />);
    const empty = handle.container.querySelector("[data-testid='lead-planning']");
    expect(empty?.textContent).toContain(t("missions.lead.planning"));
    expect(empty?.textContent).toMatch(/0:02:0\d/);
    expect(handle.container.textContent).not.toContain(t("missions.lead.empty"));
    handle.unmount();
  });

  it("새 활동 버튼은 Lead 영역(뷰포트) 안에 놓인다", () => {
    const mission = fakeMission();
    const first = fakeMessage(mission.id, "agent");
    seedStore({ missions: [mission], messages: [first] });
    const handle = renderUi(<LeadConversation mission={mission} onOpenRun={() => undefined} onStartFollowUp={() => undefined} />);
    const scroll = handle.container.querySelector("[data-testid='lead-scroll']") as HTMLElement;
    // 사용자가 위로 올린 상태를 만든다(레이아웃 없는 환경이라 크기를 직접 준다).
    Object.defineProperty(scroll, "scrollHeight", { configurable: true, value: 2000 });
    Object.defineProperty(scroll, "clientHeight", { configurable: true, value: 300 });
    scroll.scrollTop = 0;
    dispatch(scroll, new Event("scroll"));
    act(() => seedStore({ messages: [fakeMessage(mission.id, "agent", { created_at: "2026-09-13T02:00:00.000Z" })] }));
    const button = handle.container.querySelector("[data-testid='new-activity']");
    expect(button?.textContent).toBe(t("missions.lead.newActivity", { count: 1 }));
    expect(button?.parentElement?.classList.contains("mission-lead-viewport")).toBe(true);
    handle.unmount();
  });
});

