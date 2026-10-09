import { beforeEach, expect, it, vi } from "vitest";
import type { Finding } from "../../generated/Finding";
import { FindingReview } from "./FindingReview";
import { uploadTextArtifact } from "./artifactUpload";
import { t, useI18nStore } from "../../i18n";
import { click, fakeMission, fakeRef, flushAsync, installMockClient, renderUi, resetAllMissionState, setValue } from "./testSupport";

vi.mock("./artifactUpload", async original => ({ ...await original<typeof import("./artifactUpload")>(), uploadTextArtifact: vi.fn() }));

beforeEach(() => { resetAllMissionState(); vi.mocked(uploadTextArtifact).mockReset(); });

function setup() {
  const client = installMockClient();
  const mission = fakeMission({ phase: "reviewing", candidate_id: "current-candidate" });
  const finding: Finding = { id: "current-finding", mission_id: mission.id, candidate_id: mission.candidate_id!, reviewer_run_id: "reviewer",
    severity: "major", path: "api.txt", line: 1, evidence_ref: fakeRef(), requirement_id: null, resolution: "open", resolution_ref: null };
  const reference = fakeRef();
  vi.mocked(uploadTextArtifact).mockResolvedValue(reference);
  const resolve = vi.spyOn(client, "missionFindingResolve").mockResolvedValue({ mission_id: mission.id, revision: "6", event_seq: "6", entity_ids: [finding.id] });
  const onRefresh = vi.fn();
  const props = { mission, finding, disabled: false, onRefresh };
  return { client, props, reference, resolve };
}

it.each(["ko", "en"] as const)("%s 근거를 같은 미션에 저장한 뒤 현재 지적을 해제한다", async language => {
  useI18nStore.setState({ language });
  const { client, props, reference, resolve } = setup();
  const ui = renderUi(<FindingReview {...props} />);
  const button = ui.container.querySelector<HTMLButtonElement>('[data-testid="finding-dismiss"]')!;
  expect(button.disabled).toBe(true);
  const field = ui.container.querySelector<HTMLTextAreaElement>("textarea")!;
  expect(ui.container.querySelector("label")?.htmlFor).toBe(field.id);
  setValue(field, "  This behavior is required by the public contract.  ");
  click(button);
  await flushAsync();
  expect(uploadTextArtifact).toHaveBeenCalledTimes(1);
  const upload = vi.mocked(uploadTextArtifact).mock.calls[0];
  expect(upload[0] === client).toBe(true);
  expect(upload[1]).toBe("This behavior is required by the public contract.");
  expect(upload[2]).toMatchObject({ missionId: props.mission.id });
  expect(resolve).toHaveBeenCalledTimes(1);
  expect(resolve).toHaveBeenCalledWith(expect.objectContaining({ mission_id: props.mission.id, finding_id: props.finding.id,
    expected_revision: props.mission.revision, resolution: "dismissed", reason_ref: reference }));
  expect(props.onRefresh).toHaveBeenCalledTimes(1);
  expect(ui.container.querySelector('[data-testid="finding-saved"]')?.textContent).toBe(t("missions.finding.saved"));
  ui.unmount();
});

it("응답이 끊긴 저장은 사유와 요청 ID를 유지하여 명시적으로 재시도한다", async () => {
  const { props, resolve } = setup();
  resolve.mockRejectedValueOnce(new Error("receipt unavailable"));
  const ui = renderUi(<FindingReview {...props} />);
  const field = ui.container.querySelector<HTMLTextAreaElement>("textarea")!;
  setValue(field, "Reviewed and justified.");
  click(ui.container.querySelector('[data-testid="finding-dismiss"]')!);
  await flushAsync();
  expect(field.value).toBe("Reviewed and justified.");
  expect(ui.container.querySelector('[role="alert"]')?.textContent).toContain("receipt unavailable");
  expect(ui.container.querySelector('[data-testid="finding-saved"]')).toBeNull();
  click(ui.container.querySelector('[data-testid="finding-dismiss"]')!);
  await flushAsync();
  expect(resolve.mock.calls[1][0]).toEqual(resolve.mock.calls[0][0]);
  expect(uploadTextArtifact).toHaveBeenCalledTimes(1);
  ui.unmount();
});

it("사유 업로드 중 후보가 바뀌면 이전 지적 해제를 전송하지 않는다", async () => {
  const { props, reference, resolve } = setup();
  let finish!: (ref: typeof reference) => void;
  vi.mocked(uploadTextArtifact).mockReturnValue(new Promise(res => { finish = res; }));
  const ui = renderUi(<FindingReview {...props} />);
  setValue(ui.container.querySelector("textarea")!, "The reviewed candidate is correct.");
  click(ui.container.querySelector('[data-testid="finding-dismiss"]')!);
  ui.rerender(<FindingReview {...props} mission={{ ...props.mission, candidate_id: "new-candidate" }} />);
  finish(reference);
  await flushAsync();
  expect(resolve).not.toHaveBeenCalled();
  expect(ui.container.querySelector('[role="alert"]')?.textContent).toContain(t("missions.finding.changed"));
  ui.unmount();
});

it.each([
  ["ko", "blocking", "차단"],
  ["ko", "major", "주요"],
  ["ko", "minor", "보통"],
  ["ko", "note", "참고"],
  ["en", "blocking", "Blocking"],
  ["en", "note", "Note"],
] as const)("%s 심각도 %s는 원문 enum 대신 '%s' 라벨로 보인다", (language, severity, label) => {
  useI18nStore.setState({ language });
  const { props } = setup();
  const ui = renderUi(<FindingReview {...props} finding={{ ...props.finding, severity }} />);
  const shown = ui.container.querySelector('[data-testid="finding-severity"]');
  expect(shown?.textContent).toBe(label);
  expect(shown?.textContent).toBe(t(`missions.severity.${severity}`));
  expect(ui.container.querySelector('[data-testid="finding-review"]')?.getAttribute("data-anchor")).toBe(`finding:${props.finding.id}`);
  ui.unmount();
});

it("완료 미션과 다른 미션·후보의 지적은 해제 입력을 제공하지 않는다", () => {
  const { props } = setup();
  for (const changed of [
    { ...props, mission: { ...props.mission, state: "failed" as const } },
    { ...props, finding: { ...props.finding, mission_id: "other-mission" } },
    { ...props, finding: { ...props.finding, candidate_id: "old-candidate" } },
    { ...props, disabled: true },
  ]) {
    const ui = renderUi(<FindingReview {...changed} />);
    expect(ui.container.querySelector("textarea")).toBeNull();
    ui.unmount();
  }
});
