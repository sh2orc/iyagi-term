import { act } from "react";
import { beforeEach, expect, it, vi } from "vitest";
import { MessageComposer } from "./MessageComposer";
import { MessageRecovery } from "./MessageRecovery";
import { getMessageJournal, messageScope, setMessageJournalForTests } from "./messageJournal";
import { resetMessageDraftsForTests } from "./messageSubmission";
import { uploadTextArtifact } from "./artifactUpload";
import { t } from "../../i18n";
import { click, fakeMessage, fakeMission, flushAsync, installMockClient, memoryMessageJournalForTests, renderUi, resetAllMissionState, seedStore, setValue } from "./testSupport";

beforeEach(resetAllMissionState);

async function setup(replacement = false) {
  const client = installMockClient();
  const template = fakeMission();
  await client.missionCreate({ request_id: crypto.randomUUID(), title: template.title, repository_path: template.repository_path,
    expected_base_oid: template.base_oid, goal_ref: template.goal_ref, requirements: template.requirements,
    policy: template.policy, role_bindings: template.role_bindings });
  const mission = client.missions[0];
  const source = fakeMessage(mission.id, "user", { delivery: "unknown" });
  if (replacement) client.seedMissionEntity(mission.id, { kind: "message", value: source });
  seedStore({ missions: [mission], messages: replacement ? [source] : [] });
  const scope = messageScope(mission.id, null, replacement ? source.id : null);
  const render = () => replacement ? <MessageRecovery mission={mission} message={source} text="Earlier instruction." bodyError={false} />
    : <MessageComposer mission={mission} recipient="Lead" placeholder="Instruction" />;
  const mount = async (open = true) => {
    const ui = renderUi(render()); await flushAsync(6);
    if (replacement && open) {
      const button = ui.container.querySelector('[data-testid="message-replace-open"]');
      if (button) click(button);
    }
    return ui;
  };
  const input = (ui: ReturnType<typeof renderUi>) => ui.container.querySelector<HTMLTextAreaElement>("textarea")!;
  const button = (ui: ReturnType<typeof renderUi>) => ui.container.querySelector<HTMLButtonElement>(`[data-testid="${replacement ? "message-replacement-send" : "lead-send"}"]`)!;
  const snapshot = () => client.missionSnapshot({ mission_id: mission.id, snapshot_id: null, cursor: null });
  return { client, mission, source, scope, render, mount, input, button, snapshot };
}

it.each([false, true])("재시작 복원은 원문 대신 요청 참조를 읽고 같은 요청만 확인한다: replacement=%s", async replacement => {
  const s = await setup(replacement), journal = getMessageJournal();
  const actual = s.client.missionMessage.bind(s.client);
  const send = vi.spyOn(s.client, "missionMessage").mockImplementationOnce(async params => { await actual(params); throw new Error("lost receipt"); });
  const begin = vi.spyOn(s.client, "artifactBegin");
  let ui = await s.mount();
  const text = "재시작해도 외부 요청을 중복 생성하지 마세요. 😀";
  setValue(s.input(ui), text); click(s.button(ui)); await flushAsync(12);
  const params = send.mock.calls[0][0];
  expect(await journal.load(s.scope)).toEqual(params);
  expect(JSON.stringify(await journal.load(s.scope))).not.toContain(text);
  const before = await s.snapshot();
  ui.unmount(); resetMessageDraftsForTests();
  const read = vi.spyOn(s.client, "artifactRead");
  ui = await s.mount(false);
  expect(s.input(ui).value).toBe(text); expect(s.input(ui).disabled).toBe(true);
  expect(read).toHaveBeenCalled(); expect(send).toHaveBeenCalledTimes(1); // Restore never sends.
  click(s.button(ui)); await flushAsync(12);
  expect(send.mock.calls[1][0]).toEqual(params);
  expect(begin).toHaveBeenCalledTimes(1);
  expect(await journal.load(s.scope)).toBeNull();
  expect((await s.snapshot()).entities).toEqual(before.entities);
  ui.unmount();
});

it("전송 전 복구 저장이 끝나야 RPC를 시작하며 실패하면 미전송 요청을 유지한다", async () => {
  const s = await setup(), journal = getMessageJournal();
  const actualSave = journal.save.bind(journal);
  let reject!: (error: Error) => void;
  const held = new Promise<void>((_, r) => { reject = r; });
  vi.spyOn(journal, "save").mockImplementationOnce(() => held);
  const send = vi.spyOn(s.client, "missionMessage");
  const ui = await s.mount();
  setValue(s.input(ui), "기록을 저장한 후 보내세요."); click(s.button(ui)); await flushAsync(8);
  expect(send).not.toHaveBeenCalled(); expect(s.button(ui).disabled).toBe(true);
  reject(new Error("quota exhausted")); await flushAsync(5);
  expect(send).not.toHaveBeenCalled(); expect(await journal.load(s.scope)).toBeNull();
  expect(s.input(ui).disabled).toBe(true);
  const save = vi.mocked(journal.save); const original = save.mock.calls[0][0];
  save.mockImplementation(actualSave);
  click(s.button(ui)); await flushAsync(12);
  expect(send).toHaveBeenCalledTimes(1); expect(send).toHaveBeenCalledWith(original);
  expect(await journal.load(s.scope)).toBeNull(); ui.unmount();
});

it("성공 응답 뒤 복구 기록 정리에 실패해도 재시작 후 메시지를 다시 생성하지 않는다", async () => {
  const s = await setup(), journal = getMessageJournal();
  vi.spyOn(journal, "forget").mockRejectedValueOnce(new Error("journal cleanup failed"));
  const send = vi.spyOn(s.client, "missionMessage");
  let ui = await s.mount();
  setValue(s.input(ui), "기록 정리 장애"); click(s.button(ui)); await flushAsync(12);
  const first = send.mock.calls[0][0];
  expect(await journal.load(s.scope)).toEqual(first);
  expect(s.input(ui).disabled).toBe(true);
  ui.unmount(); resetMessageDraftsForTests(); ui = await s.mount();
  expect(s.input(ui).value).toBe("기록 정리 장애");
  click(s.button(ui)); await flushAsync(12);
  expect(send.mock.calls[1][0]).toEqual(first);
  const messages = (await s.snapshot()).entities.flatMap(e => e.kind === "message" ? [e.value] : []);
  expect(messages).toHaveLength(1); expect(await journal.load(s.scope)).toBeNull(); ui.unmount();
});

it("손상되거나 읽을 수 없는 기록은 입력을 잠그고 명시적 재조회로 복구한다", async () => {
  const s = await setup(), journal = getMessageJournal();
  vi.spyOn(journal, "load").mockRejectedValueOnce(new Error("invalid stored record"));
  const send = vi.spyOn(s.client, "missionMessage");
  const ui = await s.mount();
  expect(s.input(ui).disabled).toBe(true); expect(s.button(ui).disabled).toBe(true);
  expect(ui.container.querySelector('[role="alert"]')?.textContent).toContain("invalid stored record");
  const retry = [...ui.container.querySelectorAll("button")].find(b => b.textContent === t("missions.composer.restoreRetry"))!;
  click(retry); await flushAsync(5);
  expect(s.input(ui).disabled).toBe(false); expect(send).not.toHaveBeenCalled(); ui.unmount();
});

it("서버 거절 뒤 기록 삭제가 실패하면 원래 요청을 유지하고 정리 후에만 편집한다", async () => {
  const s = await setup(), journal = getMessageJournal();
  const actual = s.client.missionMessage.bind(s.client);
  const send = vi.spyOn(s.client, "missionMessage").mockImplementationOnce(async params => {
    s.client.seedMissionEntities(s.mission.id, [{ kind: "mission", value: { ...s.mission } }]);
    return actual(params); // The server rejects the previous revision.
  });
  vi.spyOn(journal, "forget").mockRejectedValueOnce(new Error("delete failed"));
  let ui = await s.mount();
  setValue(s.input(ui), "거절된 지시를 수정할 초안"); click(s.button(ui)); await flushAsync(12);
  const request = send.mock.calls[0][0];
  expect(s.input(ui).disabled).toBe(true); expect(await journal.load(s.scope)).toEqual(request);
  ui.unmount(); resetMessageDraftsForTests(); ui = await s.mount();
  click(s.button(ui)); await flushAsync(8);
  expect(send.mock.calls[1][0]).toEqual(request);
  expect(await journal.load(s.scope)).toBeNull(); expect(s.input(ui).disabled).toBe(false);
  expect(s.input(ui).value).toBe("거절된 지시를 수정할 초안");
  expect((await s.snapshot()).entities.flatMap(e => e.kind === "message" ? [e.value] : [])).toHaveLength(0);
  ui.unmount();
});

it("다른 창의 미확인 요청을 덮어쓰지 않고 복원하며 이 창의 초안은 뒤에 유지한다", async () => {
  const s = await setup(), journal = getMessageJournal();
  const ui = await s.mount();
  const ref = await uploadTextArtifact(s.client, "다른 창의 지시", { missionId: s.mission.id });
  const other = { request_id: crypto.randomUUID(), mission_id: s.mission.id, expected_revision: s.mission.revision,
    target_task_id: null, body_ref: ref };
  // Another UI window reserves the same recipient after this UI hydrated empty.
  await journal.save(other);
  const send = vi.spyOn(s.client, "missionMessage");
  setValue(s.input(ui), "이 창의 새 초안"); click(s.button(ui)); await flushAsync(12);
  expect(send).not.toHaveBeenCalled(); expect(await journal.load(s.scope)).toEqual(other);
  expect(s.input(ui).value).toBe("다른 창의 지시");
  click(s.button(ui)); await flushAsync(12);
  expect(send).toHaveBeenCalledTimes(1); expect(send).toHaveBeenCalledWith(other);
  expect(s.input(ui).value).toBe("이 창의 새 초안"); expect(s.input(ui).disabled).toBe(false); ui.unmount();
});

it("본문 해시가 달라지면 내용을 표시하지 않고 원래 요청 참조로 응답만 확인한다", async () => {
  const s = await setup(), journal = memoryMessageJournalForTests();
  setMessageJournalForTests(journal);
  const ref = await uploadTextArtifact(s.client, "Original text", { missionId: s.mission.id });
  const params = { request_id: crypto.randomUUID(), mission_id: s.mission.id, expected_revision: s.mission.revision,
    target_task_id: null, body_ref: ref };
  await s.client.missionMessage(params); await journal.save(params);
  vi.spyOn(s.client, "artifactRead").mockResolvedValue({ data_b64: btoa("Tampered text"), next_offset: ref.bytes, complete: true });
  const send = vi.spyOn(s.client, "missionMessage");
  const ui = await s.mount();
  expect(s.input(ui).value).toBe(""); expect(s.input(ui).disabled).toBe(true);
  expect(ui.container.textContent).toContain(t("missions.composer.bodyUnavailable"));
  expect(ui.container.textContent).not.toContain("Tampered text");
  click(s.button(ui)); await flushAsync(8);
  expect(send).toHaveBeenCalledTimes(1); expect(send).toHaveBeenCalledWith(params); expect(await journal.load(s.scope)).toBeNull(); ui.unmount();
});

it("이미 완료된 미션이 복원 요청을 거절해도 오류와 본문을 숨기지 않는다", async () => {
  const s = await setup(), journal = getMessageJournal();
  const ref = await uploadTextArtifact(s.client, "아직 저장되지 않은 지시", { missionId: s.mission.id });
  await journal.save({ request_id: crypto.randomUUID(), mission_id: s.mission.id, expected_revision: s.mission.revision,
    target_task_id: null, body_ref: ref });
  s.client.seedMissionEntities(s.mission.id, [{ kind: "mission", value: { ...s.mission, state: "completed" } }]);
  const completed = s.client.missions[0]; seedStore({ missions: [completed] });
  const followUp = vi.fn();
  const ui = renderUi(<MessageComposer mission={completed} recipient="Lead" placeholder="Instruction" onStartFollowUp={followUp} />);
  await flushAsync(6); expect(s.input(ui).value).toBe("아직 저장되지 않은 지시");
  click(s.button(ui)); await flushAsync(8);
  expect(await journal.load(s.scope)).toBeNull();
  expect(ui.container.querySelector('[role="alert"]')).not.toBeNull();
  expect(s.input(ui).value).toBe("아직 저장되지 않은 지시"); expect(s.input(ui).disabled).toBe(true);
  click([...ui.container.querySelectorAll("button")].find(b => b.textContent === t("missions.lead.followUp"))!);
  expect(followUp).toHaveBeenCalledTimes(1); ui.unmount();
});

it("이전 복구 읽기가 늦게 끝나도 새 화면 상태를 덮어쓰지 않는다", async () => {
  const s = await setup(), journal = getMessageJournal();
  let release!: (value: null) => void;
  vi.spyOn(journal, "load").mockImplementationOnce(() => new Promise(r => { release = r; }));
  const first = renderUi(s.render());
  expect(s.input(first).disabled).toBe(true);
  first.unmount(); act(() => resetMessageDraftsForTests());
  const next = await s.mount(); setValue(s.input(next), "새 화면의 초안");
  release(null); await flushAsync(5);
  expect(s.input(next).value).toBe("새 화면의 초안"); next.unmount();
});
