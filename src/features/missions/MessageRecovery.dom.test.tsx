import { beforeEach, expect, it, vi } from "vitest";
import { MessageRecovery } from "./MessageRecovery";
import { uploadTextArtifact } from "./artifactUpload";
import { t, useI18nStore } from "../../i18n";
import { click, fakeDecision, fakeMessage, fakeMission, fakeRef, fakeTask, flushAsync, installMockClient, renderUi, resetAllMissionState, seedStore, setValue } from "./testSupport";

vi.mock("./artifactUpload", async original => ({ ...await original<typeof import("./artifactUpload")>(), uploadTextArtifact: vi.fn() }));
beforeEach(() => { resetAllMissionState(); vi.mocked(uploadTextArtifact).mockReset(); });

function setup() {
  const client = installMockClient();
  const mission = fakeMission();
  const message = fakeMessage(mission.id, "user", { delivery: "unknown" });
  const reference = fakeRef();
  vi.mocked(uploadTextArtifact).mockResolvedValue(reference);
  const send = vi.spyOn(client, "missionMessage").mockResolvedValue({ mission_id: mission.id, revision: "6", event_seq: "6", entity_ids: ["replacement"] });
  seedStore({ missions: [mission], messages: [message] });
  return { client, props: { mission, message, text: "Original instruction.", bodyError: false }, reference, send };
}
async function open(ui: ReturnType<typeof renderUi>) {
  await flushAsync(3);
  click(ui.container.querySelector('[data-testid="message-replace-open"]')!);
  return ui.container.querySelector<HTMLTextAreaElement>("textarea")!;
}
function submit(ui: ReturnType<typeof renderUi>) { click(ui.container.querySelector('[data-testid="message-replacement-send"]')!); }

it.each(["ko", "en"] as const)("%s 내용을 확인하고 원본을 연결한 새 메시지를 보낸다", async language => {
  useI18nStore.setState({ language });
  const { client, props, reference, send } = setup();
  const ui = renderUi(<MessageRecovery {...props} />);
  expect(ui.container.querySelector("textarea")).toBeNull();
  const field = await open(ui);
  expect(field.value).toBe("Original instruction.");
  expect(ui.container.querySelector("label")?.htmlFor).toBe(field.id);
  expect(ui.container.textContent).toContain(t("missions.messageRecovery.unknown"));
  setValue(field, "  Reviewed replacement.  ");
  submit(ui);
  await flushAsync();
  const upload = vi.mocked(uploadTextArtifact).mock.calls[0];
  expect(upload[0] === client).toBe(true);
  expect(upload[1]).toBe("Reviewed replacement.");
  expect(upload[2]).toMatchObject({ missionId: props.mission.id });
  expect(send).toHaveBeenCalledWith(expect.objectContaining({ mission_id: props.mission.id, expected_revision: props.mission.revision,
    target_task_id: null, body_ref: reference, supersedes_message_id: props.message.id }));
  expect(props.message.delivery).toBe("unknown");
  expect(ui.container.querySelector('[role="status"]')?.textContent).toBe(t("missions.messageRecovery.saved"));
  ui.unmount();
});

it("응답 불명에는 본문과 요청 ID를 유지하고 같은 요청으로 결과를 확인한다", async () => {
  const { props, send } = setup();
  send.mockRejectedValueOnce(new Error("receipt unavailable"));
  const ui = renderUi(<MessageRecovery {...props} />);
  const field = await open(ui);
  setValue(field, "Reviewed once.");
  submit(ui);
  await flushAsync();
  expect(field.disabled).toBe(true);
  expect(field.value).toBe("Reviewed once.");
  expect(ui.container.querySelector('[data-testid="message-replacement-send"]')?.textContent).toBe(t("missions.messageRecovery.checkReceipt"));
  // Terminal state must still permit resolving the exact existing receipt.
  ui.rerender(<MessageRecovery {...props} mission={{ ...props.mission, revision: "8", state: "cancelled" }} />);
  submit(ui);
  await flushAsync();
  expect(send.mock.calls[1][0]).toEqual(send.mock.calls[0][0]);
  expect(uploadTextArtifact).toHaveBeenCalledTimes(1);
  ui.unmount();
});

it("확인된 revision 거절은 최신 revision으로만 새 요청을 만든다", async () => {
  const { props, send } = setup();
  send.mockRejectedValueOnce({ code: "REVISION_CONFLICT", message: "refresh" });
  const ui = renderUi(<MessageRecovery {...props} />);
  await open(ui); submit(ui);
  await flushAsync();
  seedStore({ missions: [{ ...props.mission, revision: "9" }] });
  ui.rerender(<MessageRecovery {...props} mission={{ ...props.mission, revision: "9" }} />);
  submit(ui); await flushAsync();
  expect(send.mock.calls[1][0].request_id).not.toBe(send.mock.calls[0][0].request_id);
  expect(send.mock.calls[1][0].expected_revision).toBe("9");
  expect(uploadTextArtifact).toHaveBeenCalledTimes(1);
  ui.unmount();
});

it("업로드 도중 다른 클라이언트가 대체하면 두 번째 메시지를 보내지 않는다", async () => {
  const { props, reference, send } = setup();
  let finish!: (ref: typeof reference) => void;
  vi.mocked(uploadTextArtifact).mockReturnValue(new Promise(resolve => { finish = resolve; }));
  const ui = renderUi(<MessageRecovery {...props} />);
  await open(ui); submit(ui);
  const replacement = fakeMessage(props.mission.id, "user", { supersedes_message_id: props.message.id });
  ui.rerender(<MessageRecovery {...props} replacement={replacement} />);
  finish(reference); await flushAsync();
  expect(send).not.toHaveBeenCalled();
  expect(ui.container.querySelector('[data-testid="message-replacement-link"]')).not.toBeNull();
  expect(ui.container.querySelector('[role="alert"]')?.textContent).toContain(t("missions.messageRecovery.changed"));
  ui.unmount();
});

it("종료 상태·확인된 전달·다른 미션·승인 답변에는 재전송을 제공하지 않는다", () => {
  const { props } = setup();
  for (const changed of [
    { ...props, mission: { ...props.mission, state: "cancelled" as const } },
    { ...props, message: { ...props.message, delivery: "delivered" as const } },
    { ...props, message: { ...props.message, delivery: "queued" as const } },
    { ...props, message: { ...props.message, mission_id: "another-mission" } },
    { ...props, message: { ...props.message, role: "agent" as const } },
    { ...props, message: { ...props.message, target_task_id: "missing-task" } },
  ]) {
    const ui = renderUi(<MessageRecovery {...changed} />);
    expect(ui.container.querySelector('[data-testid="message-replace-open"]')).toBeNull(); ui.unmount();
  }
  const decision = fakeDecision(props.mission.id, { kind: "approval", answer_message_id: props.message.id, state: "answered" });
  seedStore({ decisions: [decision] });
  const ui = renderUi(<MessageRecovery {...props} />);
  expect(ui.container.querySelector('[data-testid="message-replace-open"]')).toBeNull(); ui.unmount();
});

it("작업 수신자와 UTF-8 상한을 유지하고 빈 메시지를 차단한다", async () => {
  const { props, send } = setup();
  const task = fakeTask(props.mission.id, "running");
  seedStore({ tasks: [task] });
  const ui = renderUi(<MessageRecovery {...props} message={{ ...props.message, target_task_id: task.id }} />);
  const field = await open(ui);
  expect(ui.container.querySelector("label")?.textContent).toContain(task.title);
  setValue(field, " ");
  expect(ui.container.querySelector<HTMLButtonElement>('[data-testid="message-replacement-send"]')?.disabled).toBe(true);
  setValue(field, "한".repeat(90000));
  expect(ui.container.querySelector<HTMLButtonElement>('[data-testid="message-replacement-send"]')?.disabled).toBe(true);
  expect(uploadTextArtifact).not.toHaveBeenCalled();
  setValue(field, "Reviewed task instruction."); submit(ui); await flushAsync();
  expect(send.mock.calls[0][0].target_task_id).toBe(task.id); ui.unmount();
});
