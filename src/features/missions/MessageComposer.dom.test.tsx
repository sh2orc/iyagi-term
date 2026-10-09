import { act } from "react";
import { beforeEach, expect, it, vi } from "vitest";
import type { MissionMessageParams } from "../../generated/MissionMessageParams";
import { t, useI18nStore } from "../../i18n";
import { usePreferences } from "../../store/preferences";
import { LeadConversation } from "./LeadConversation";
import { MessageComposer } from "./MessageComposer";
import { RunDetail } from "./RunDetail";
import { MISSION_TEXT_MAX_BYTES } from "./artifactUpload";
import { click, fakeMission, fakeTask, flushAsync, installMockClient, pressKey, renderUi, resetAllMissionState, seedStore, setValue } from "./testSupport";

beforeEach(() => { resetAllMissionState(); usePreferences.setState({ missionEnterSend: true }); });

async function setup(recipient: "lead" | "task") {
  const client = installMockClient();
  const template = fakeMission();
  await client.missionCreate({ request_id: crypto.randomUUID(), title: template.title, repository_path: template.repository_path,
    expected_base_oid: template.base_oid, goal_ref: template.goal_ref, requirements: template.requirements,
    policy: template.policy, role_bindings: template.role_bindings });
  const mission = client.missions[0];
  const task = fakeTask(mission.id, "ready");
  client.seedMissionEntity(mission.id, { kind: "task", value: task });
  seedStore({ missions: [mission], tasks: [task] });
  const render = (m = mission, target = task) => recipient === "lead"
    ? <LeadConversation mission={m} onOpenRun={() => undefined} onStartFollowUp={() => undefined} />
    : <RunDetail mission={m} task={target} detailTab="activity" onDetailTabChange={() => undefined} />;
  const mount = async () => {
    const ui = renderUi(render());
    if (recipient === "task") click(ui.container.querySelector('[data-testid="instruct-toggle"]')!);
    await flushAsync(3);
    return ui;
  };
  const input = (ui: ReturnType<typeof renderUi>) => ui.container.querySelector<HTMLTextAreaElement>(`[data-testid="${recipient === "lead" ? "lead-composer" : "task-composer-input"}"]`)!;
  const button = (ui: ReturnType<typeof renderUi>) => ui.container.querySelector<HTMLButtonElement>(`[data-testid="${recipient === "lead" ? "lead-send" : "task-composer-send"}"]`)!;
  const messages = async () => (await client.missionSnapshot({ mission_id: mission.id, snapshot_id: null, cursor: null })).entities.flatMap(e => e.kind === "message" ? [e.value] : []);
  return { client, mission, task, render, mount, input, button, messages };
}

it.each(["lead", "task"] as const)("%s 저장 응답 손실 뒤 화면을 다시 열고 종료 상태에서도 같은 요청을 확인한다", async recipient => {
  const s = await setup(recipient);
  const actual = s.client.missionMessage.bind(s.client);
  const send = vi.spyOn(s.client, "missionMessage").mockImplementationOnce(async params => {
    await actual(params); throw new Error("lost receipt");
  });
  const begin = vi.spyOn(s.client, "artifactBegin");
  let ui = await s.mount();
  setValue(s.input(ui), "이미 작성한 외부 요청을 검토해 주세요.");
  act(() => { s.button(ui).click(); s.button(ui).click(); });
  await flushAsync(15);
  expect(send).toHaveBeenCalledTimes(1);
  expect(s.input(ui).disabled).toBe(true);
  expect(s.button(ui).textContent).toBe(t("missions.messageRecovery.checkReceipt"));
  expect(ui.container.querySelector('[role="alert"]')?.textContent).toBe(t("missions.composer.receiptError", { message: "lost receipt" }));
  expect((await s.messages()).length).toBe(1);
  ui.unmount();
  ui = await s.mount();
  expect(s.input(ui).value).toBe("이미 작성한 외부 요청을 검토해 주세요.");
  expect(s.input(ui).disabled).toBe(true);
  const completed = { ...s.client.missions[0], state: "completed" as const, revision: "20" };
  const stoppedTask = { ...s.task, state: "succeeded" as const };
  seedStore({ missions: [completed], tasks: [stoppedTask] });
  ui.rerender(s.render(completed, stoppedTask));
  expect(s.button(ui).disabled).toBe(false);
  click(s.button(ui)); await flushAsync(15);
  expect(send.mock.calls[1][0]).toEqual(send.mock.calls[0][0]);
  expect(begin).toHaveBeenCalledTimes(1);
  expect((await s.messages()).length).toBe(1);
  expect(ui.container.querySelector('[data-testid="message-receipt-note"]')).toBeNull();
  if (recipient === "lead") expect(ui.container.textContent).toContain(t("missions.lead.followUp"));
  else expect(s.input(ui).value).toBe("");
  ui.unmount();
});

it.each(["lead", "task"] as const)("%s 확인된 revision 거절 후에만 수정과 새 요청을 허용한다", async recipient => {
  const s = await setup(recipient);
  const send = vi.spyOn(s.client, "missionMessage").mockRejectedValueOnce({ code: "REVISION_CONFLICT", message: "stale" });
  const ui = await s.mount();
  setValue(s.input(ui), "처음 입력"); click(s.button(ui)); await flushAsync(12);
  expect(s.input(ui).disabled).toBe(false);
  expect(s.button(ui).textContent).toBe(t("missions.lead.send"));
  s.client.seedMissionEntities(s.mission.id, [{ kind: "mission", value: { ...s.mission } }]);
  seedStore({ missions: [s.client.missions[0]] });
  setValue(s.input(ui), "검토한 새 입력"); click(s.button(ui)); await flushAsync(12);
  const first = send.mock.calls[0][0], next = send.mock.calls[1][0];
  expect(first.request_id).not.toBe(next.request_id);
  expect(first.body_ref.id).not.toBe(next.body_ref.id);
  expect(next.expected_revision).toBe(String(BigInt(s.mission.revision) + 1n));
  expect(next.target_task_id).toBe(recipient === "lead" ? null : s.task.id);
  expect((await s.messages()).length).toBe(1);
  expect(s.input(ui).value).toBe(""); ui.unmount();
});

it("저장 여부를 모르는 오류는 자동 재시도하지 않고 원래 revision도 유지한다", async () => {
  const s = await setup("lead");
  const send = vi.spyOn(s.client, "missionMessage").mockRejectedValueOnce({ code: "STORAGE_UNAVAILABLE", message: "commit uncertain" });
  const ui = await s.mount();
  setValue(s.input(ui), "예산을 먼저 확인하세요."); click(s.button(ui)); await flushAsync(12);
  expect(send).toHaveBeenCalledTimes(1);
  const request = send.mock.calls[0][0];
  s.client.seedMissionEntities(s.mission.id, [{ kind: "mission", value: { ...s.mission } }]);
  seedStore({ missions: [s.client.missions[0]] });
  click(s.button(ui)); await flushAsync(12); // exact old request: daemon rejects its old revision
  expect(send.mock.calls[1][0]).toEqual(request);
  expect(s.input(ui).disabled).toBe(false);
  expect((await s.messages()).length).toBe(0);
  click(s.button(ui)); await flushAsync(12);
  expect(send.mock.calls[2][0].request_id).not.toBe(request.request_id);
  expect(send.mock.calls[2][0].body_ref).toEqual(request.body_ref);
  expect((await s.messages()).length).toBe(1); ui.unmount();
});

it("본문 업로드 응답 손실은 같은 업로드 ID로 복구하고 메시지는 한 번만 만든다", async () => {
  const s = await setup("lead");
  const actualCommit = s.client.artifactCommit.bind(s.client);
  vi.spyOn(s.client, "artifactCommit").mockImplementationOnce(async params => {
    await actualCommit(params); throw new Error("upload receipt lost");
  });
  const begin = vi.spyOn(s.client, "artifactBegin");
  const send = vi.spyOn(s.client, "missionMessage");
  const ui = await s.mount();
  setValue(s.input(ui), "같은 내용"); click(s.button(ui)); await flushAsync(12);
  expect(send).not.toHaveBeenCalled();
  expect(s.input(ui).disabled).toBe(false);
  click(s.button(ui)); await flushAsync(12);
  expect(begin.mock.calls[1][0]).toEqual(begin.mock.calls[0][0]);
  expect(send).toHaveBeenCalledTimes(1);
  expect((await s.messages()).length).toBe(1); ui.unmount();
});

it("업로드 중 수신 작업을 바꿔도 지시와 응답은 원래 작업에만 적용한다", async () => {
  const s = await setup("task");
  const second = fakeTask(s.mission.id, "ready", { title: "다른 담당" });
  s.client.seedMissionEntity(s.mission.id, { kind: "task", value: second });
  seedStore({ tasks: [second] });
  const commit = s.client.artifactCommit.bind(s.client);
  let release!: () => void;
  const wait = new Promise<void>(resolve => { release = resolve; });
  vi.spyOn(s.client, "artifactCommit").mockImplementationOnce(async params => { await wait; return commit(params); });
  const send = vi.spyOn(s.client, "missionMessage");
  const ui = await s.mount();
  setValue(s.input(ui), "첫 담당의 입력"); click(s.button(ui)); await flushAsync(5);
  ui.rerender(s.render(s.mission, second)); await flushAsync(3);
  expect(s.input(ui).value).toBe("");
  expect(s.input(ui).disabled).toBe(false);
  setValue(s.input(ui), "둘째 담당의 초안");
  release(); await flushAsync(15);
  expect(send.mock.calls[0][0].target_task_id).toBe(s.task.id);
  expect(s.input(ui).value).toBe("둘째 담당의 초안");
  ui.rerender(s.render());
  expect(s.input(ui).value).toBe("");
  ui.rerender(s.render(s.mission, second)); await flushAsync(3);
  expect(s.input(ui).value).toBe("둘째 담당의 초안"); ui.unmount();
});

it.each(["lead", "task"] as const)("%s 업로드 중 종료되면 새 메시지를 저장하지 않는다", async recipient => {
  const s = await setup(recipient);
  const commit = s.client.artifactCommit.bind(s.client);
  vi.spyOn(s.client, "artifactCommit").mockImplementationOnce(async params => {
    const ref = await commit(params);
    seedStore({ missions: [{ ...s.mission, state: "cancelled" }], tasks: [{ ...s.task, state: "succeeded" }] });
    return ref;
  });
  const send = vi.spyOn(s.client, "missionMessage");
  const ui = await s.mount();
  setValue(s.input(ui), "중단 후 실행하지 마세요."); click(s.button(ui)); await flushAsync(12);
  expect(send).not.toHaveBeenCalled();
  expect(s.input(ui).value).toBe("중단 후 실행하지 마세요.");
  expect(ui.container.querySelector('[role="alert"]')?.textContent).toContain(t("missions.composer.closed")); ui.unmount();
});

it("서로 다른 미션과 전체/개별 수신자의 초안을 섞지 않는다", async () => {
  const s = await setup("lead");
  const ui = await s.mount();
  setValue(s.input(ui), "첫 미션 전체 지시");
  const other = fakeMission(); seedStore({ missions: [other] });
  ui.rerender(s.render(other)); await flushAsync(3);
  expect(s.input(ui).value).toBe(""); setValue(s.input(ui), "다른 미션 지시");
  ui.rerender(s.render()); expect(s.input(ui).value).toBe("첫 미션 전체 지시");
  const detail = renderUi(<RunDetail mission={s.mission} task={s.task} detailTab="activity" onDetailTabChange={() => undefined} />);
  click(detail.container.querySelector('[data-testid="instruct-toggle"]')!);
  expect(detail.container.querySelector<HTMLTextAreaElement>("textarea")?.value).toBe("");
  ui.unmount(); detail.unmount();
});

it.each(["ko", "en"] as const)("%s 빈 내용·UTF-8 상한·Enter 설정을 두 입력창에 적용한다", async language => {
  const s = await setup("task"); useI18nStore.setState({ language });
  const send = vi.spyOn(s.client, "missionMessage");
  const ui = await s.mount();
  expect(s.button(ui).disabled).toBe(true);
  setValue(s.input(ui), "한".repeat(Math.floor(MISSION_TEXT_MAX_BYTES / 3) + 1));
  expect(s.button(ui).disabled).toBe(true);
  expect(ui.container.querySelector('[role="alert"]')?.textContent).toBe(t("missions.messageRecovery.tooLarge"));
  setValue(s.input(ui), "정확한 지시");
  act(() => usePreferences.setState({ missionEnterSend: false }));
  pressKey(s.input(ui), "Enter"); await flushAsync(5); expect(send).not.toHaveBeenCalled();
  pressKey(s.input(ui), "Enter", { ctrlKey: true }); await flushAsync(12);
  expect(send).toHaveBeenCalledTimes(1); ui.unmount();
});

it("동시에 열린 같은 수신자의 입력창은 하나의 전송만 시작한다", async () => {
  const s = await setup("lead");
  let resolve!: (value: Awaited<ReturnType<typeof s.client.missionMessage>>) => void;
  const send = vi.spyOn(s.client, "missionMessage").mockImplementation((_params: MissionMessageParams) => new Promise(r => { resolve = r; }));
  const first = await s.mount(), second = await s.mount();
  setValue(s.input(first), "동시에 보내지 않기");
  act(() => { s.button(first).click(); s.button(second).click(); }); await flushAsync(12);
  expect(send).toHaveBeenCalledTimes(1); expect(s.input(second).disabled).toBe(true);
  resolve({ mission_id: s.mission.id, revision: "2", event_seq: "1", entity_ids: [] }); await flushAsync();
  expect(s.input(first).value).toBe(""); expect(s.input(second).value).toBe(""); first.unmount(); second.unmount();
});

it("종료·보관·검증 작업과 다른 미션의 수신자는 새 전송을 막는다", async () => {
  const s = await setup("task");
  const send = vi.spyOn(s.client, "missionMessage");
  const begin = vi.spyOn(s.client, "artifactBegin");
  const ui = await s.mount();
  setValue(s.input(ui), "보관된 초안");
  for (const [mission, task] of [
    [{ ...s.mission, state: "stopping" as const }, s.task],
    [{ ...s.mission, state: "failed" as const }, s.task],
    [{ ...s.mission, archived_at: new Date().toISOString() }, s.task],
    [s.mission, { ...s.task, state: "succeeded" as const }],
    [s.mission, { ...s.task, kind: "verify" as const }],
    [s.mission, { ...s.task, mission_id: "another-mission" }],
  ] as const) {
    ui.rerender(s.render(mission, task));
    expect(s.input(ui).disabled).toBe(true);
    expect(s.button(ui).disabled).toBe(true);
    expect(s.input(ui).value).toBe("보관된 초안");
  }
  expect(send).not.toHaveBeenCalled(); expect(begin).not.toHaveBeenCalled(); ui.unmount();
});

it.each([
  ["completed", "missions.composer.lockedCompleted", "composer-follow-up", "missions.lead.followUp"],
  ["failed", "missions.composer.lockedFailed", "composer-same-goal", "missions.outcome.sameGoal"],
  ["cancelled", "missions.composer.lockedCancelled", "composer-same-goal", "missions.outcome.sameGoal"],
] as const)("%s 작업의 전체 입력창은 상태별 잠김 문구와 다음 작업 버튼을 보인다", async (state, lockedKey, buttonId, labelKey) => {
  const mission = fakeMission({ state });
  seedStore({ missions: [mission] });
  const followUp = vi.fn(), sameGoal = vi.fn(), openResult = vi.fn();
  const ui = renderUi(<MessageComposer mission={mission} recipient={t("missions.lead.composerLabel")} placeholder=""
    onStartFollowUp={followUp} onStartSameGoal={sameGoal} onOpenResult={openResult} />);
  await flushAsync(3);
  expect(ui.container.querySelector('[data-testid="composer-locked"]')?.textContent).toBe(t(lockedKey));
  const button = ui.container.querySelector<HTMLButtonElement>(`[data-testid="${buttonId}"]`)!;
  expect(button.textContent).toBe(t(labelKey));
  click(button);
  expect(state === "completed" ? followUp : sameGoal).toHaveBeenCalledTimes(1);
  const note = ui.container.querySelector('[data-testid="follow-up-note"]');
  if (state === "completed") {
    // 확정되지 않은 채 끝난 작업의 후속 작업은 현재 HEAD에서 시작한다 — 결과를 먼저 가져오라는 안내.
    expect(note?.textContent).toContain(t("missions.composer.followUpBaseNote"));
    click(ui.container.querySelector('[data-testid="follow-up-open-result"]')!);
    expect(openResult).toHaveBeenCalledTimes(1);
  } else {
    expect(note).toBeNull();
  }
  expect(ui.container.querySelector('[data-testid="lead-composer"]')).toBeNull();
  ui.unmount();
});

it("확정된 작업의 후속 작업 안내는 이전 확정 결과 위에서 시작한다고 알린다(확정 전 안내와 구분)", async () => {
  const mission = fakeMission({ state: "completed", phase: "done", accepted_at: "2026-09-13T03:00:00.000Z" });
  seedStore({ missions: [mission] });
  const openResult = vi.fn();
  const ui = renderUi(<MessageComposer mission={mission} recipient={t("missions.lead.composerLabel")} placeholder=""
    onStartFollowUp={vi.fn()} onOpenResult={openResult} />);
  await flushAsync(3);
  const note = ui.container.querySelector('[data-testid="follow-up-note"]');
  expect(note?.textContent).toContain(t("missions.composer.followUpAcceptedNote"));
  expect(note?.textContent).not.toContain(t("missions.composer.followUpBaseNote"));
  click(ui.container.querySelector('[data-testid="follow-up-open-result"]')!);
  expect(openResult).toHaveBeenCalledTimes(1);
  ui.unmount();
});

it("멈추는 중·보관된 작업의 입력창은 상태별 이유를 보이고 연결 끊김이면 전송을 막는다", async () => {
  const mission = fakeMission({ state: "stopping" });
  seedStore({ missions: [mission] });
  const render = (m = mission, disconnected = false) =>
    <MessageComposer mission={m} recipient={t("missions.lead.composerLabel")} placeholder="" disconnected={disconnected} />;
  const ui = renderUi(render());
  await flushAsync(3);
  expect(ui.container.querySelector('[data-testid="composer-locked"]')?.textContent).toBe(t("missions.composer.lockedStopping"));
  ui.rerender(render({ ...mission, state: "running", archived_at: "2026-09-13T02:00:00.000Z" }));
  expect(ui.container.querySelector('[data-testid="composer-locked"]')?.textContent).toBe(t("missions.composer.lockedArchived"));
  const running = { ...mission, state: "running" as const };
  ui.rerender(render(running, true));
  setValue(ui.container.querySelector<HTMLTextAreaElement>('[data-testid="lead-composer"]')!, "끊긴 동안 쓴 요청");
  expect(ui.container.querySelector<HTMLTextAreaElement>('[data-testid="lead-composer"]')!.disabled).toBe(false);
  expect(ui.container.querySelector<HTMLButtonElement>('[data-testid="lead-send"]')!.disabled).toBe(true);
  expect(ui.container.querySelector('[data-testid="composer-offline"]')?.textContent).toBe(t("missions.composer.lockedOffline"));
  ui.rerender(render(running, false));
  expect(ui.container.querySelector<HTMLButtonElement>('[data-testid="lead-send"]')!.disabled).toBe(false);
  ui.unmount();
});

