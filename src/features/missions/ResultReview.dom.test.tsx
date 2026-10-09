/**
 * ResultReview 시험(05 §9): 확정 체크리스트·직접 확인·확정 요청·결과 가져오기·버리기.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { ResultReview, type ResultReviewProps } from "./ResultReview";
import {
  click,
  compatibleBinding,
  fakeCandidate,
  fakeDecision,
  fakeMission,
  fakeRef,
  fakeRun,
  fakeTask,
  fakeVerification,
  flushAsync,
  installMockClient,
  renderUi,
  resetAllMissionState,
  seedStore,
  uploadMockText,
} from "./testSupport";
import type { InputIntegrity } from "../../generated/InputIntegrity";
import type { Mission } from "../../generated/Mission";
import type { MissionAcceptParams } from "../../generated/MissionAcceptParams";
import type { MissionControlParams } from "../../generated/MissionControlParams";
import { useMissionStore } from "./store";
import { useI18nStore, t } from "../../i18n";
import { ControllerContext } from "../../app/controllerContext";
import type { SessionController } from "../terminal/sessionController";
import type { WorkspaceCleanupParams } from "../../generated/WorkspaceCleanupParams";
import type { WorkspaceCleanupResult } from "../../generated/WorkspaceCleanupResult";
import type { WorkspaceUsageParams } from "../../generated/WorkspaceUsageParams";
import type { WorkspaceUsageResult } from "../../generated/WorkspaceUsageResult";

const MUTATION = { mission_id: "", revision: "6", event_seq: "6", entity_ids: [] };

beforeEach(() => {
  resetAllMissionState();
});

afterEach(() => {
  vi.restoreAllMocks();
});

function q<T extends HTMLElement = HTMLElement>(root: ParentNode, testId: string): T | null {
  return root.querySelector<T>(`[data-testid="${testId}"]`);
}

/** 확정 대기 + 검증(기본 observed) 통과 + 리뷰 완료 + human check 1건. */
function stage(options: { integrity?: InputIntegrity; mission?: Partial<Mission> } = {}) {
  const mission = fakeMission({ phase: "awaiting_acceptance", state: "running", ...options.mission });
  const candidate = fakeCandidate(mission.id, { created_at: "2026-09-13T02:00:00.000Z" });
  mission.candidate_id = candidate.id;
  const verifyTask = fakeTask(mission.id, "succeeded", { kind: "verify", role: null, title: "verify: 단위 시험" });
  verifyTask.contract.verification_ids = ["vcmd-1"];
  const verifyRun = fakeRun(mission.id, verifyTask, "succeeded");
  verifyTask.active_run_id = null;
  const verification = fakeVerification(mission.id, candidate.id, {
    task_id: verifyTask.id,
    run_id: verifyRun.id,
    input_integrity: options.integrity ?? "observed",
  });
  const reviewTask = fakeTask(mission.id, "succeeded", { kind: "review", role: "reviewer", title: "독립 리뷰" });
  const reviewRun = fakeRun(mission.id, reviewTask, "succeeded", {
    started_at: "2026-09-13T02:05:00.000Z",
    result_ref: fakeRef("40"),
  });
  reviewTask.active_run_id = null;
  seedStore({
    missions: [mission],
    candidates: [candidate],
    tasks: [verifyTask, reviewTask],
    runs: [verifyRun, reviewRun],
    verifications: [verification],
  });
  return { mission, candidate, verifyTask, verification, reviewTask, reviewRun };
}

function render(mission: Mission, extra: Partial<ResultReviewProps> = {}) {
  const props: ResultReviewProps = { mission, onRefresh: () => undefined, ...extra };
  return renderUi(<ResultReview {...props} />);
}

/** 활성화된 직접 확인 체크박스를 모두 체크한다. */
function checkAllConfirmations(root: HTMLElement): void {
  for (const input of root.querySelectorAll<HTMLInputElement>('[data-testid="human-confirmations"] input[type="checkbox"]')) {
    if (!input.disabled && !input.checked) click(input);
  }
}

describe("확정 체크리스트와 직접 확인", () => {
  it("human check와 관찰 검증을 모두 체크해야 확정 버튼이 열리고, 비활성 사유는 첫 미충족 항목이다", async () => {
    installMockClient();
    const { mission } = stage();
    const ui = render(mission);
    await flushAsync();
    const accept = q<HTMLButtonElement>(ui.container, "accept-button")!;
    expect(accept.disabled).toBe(true);
    // 데몬 순서: 관찰 검증 확인(step 4)이 human check(step 6)보다 먼저다.
    expect(q(ui.container, "accept-blocked-reason")?.textContent).toBe(
      t("missions.result.issue.observedUnacknowledged", { command: "단위 시험" }),
    );
    expect(q(ui.container, "checklist-confirmations")?.dataset.ok).toBe("false");

    const confirmations = q(ui.container, "human-confirmations")!;
    expect(confirmations.textContent).toContain("로그인 폼이 동작한다");
    // human check는 "남은 한계"에 섞이지 않는다.
    expect(q(ui.container, "result-limitations")).toBeNull();

    click(q(confirmations, "human-check")!);
    await flushAsync();
    expect(accept.disabled).toBe(true);
    click(q(confirmations, "observed-check")!);
    await flushAsync();
    expect(accept.disabled).toBe(false);
    expect(q(ui.container, "accept-blocked-reason")).toBeNull();
    expect(q(ui.container, "checklist-confirmations")?.dataset.ok).toBe("true");
    expect(q(ui.container, "result-status")?.textContent).toBe(t("missions.result.statusReady"));
    ui.unmount();
  });

  it("확정 요청은 체크한 확인만 싣고 확인창은 결과 요약 안에 그려진다", async () => {
    const client = installMockClient();
    const { mission, candidate, verification } = stage();
    const acceptSpy = vi.fn(async (_params: MissionAcceptParams) => ({ ...MUTATION, mission_id: mission.id }));
    client.missionAccept = acceptSpy;
    const refresh = vi.fn();
    const ui = render(mission, { onRefresh: refresh });
    await flushAsync();
    checkAllConfirmations(ui.container);
    await flushAsync();
    click(q(ui.container, "accept-button")!);
    const confirm = q(ui.container, "accept-confirm")!;
    expect(q(ui.container, "result-summary")?.contains(confirm)).toBe(true);
    expect(confirm.textContent).toContain(
      "검증 1건과 직접 확인 1건을 기록하고 이 결과를 최종으로 확정합니다. 원래 브랜치에는 자동으로 반영되지 않습니다.",
    );
    click(q(confirm, "accept-confirm-ok")!);
    await flushAsync();
    expect(acceptSpy).toHaveBeenCalledTimes(1);
    expect(acceptSpy.mock.calls[0][0]).toMatchObject({
      mission_id: mission.id,
      expected_revision: mission.revision,
      candidate_id: candidate.id,
      acknowledged_verification_ids: [verification.id],
      human_requirement_ids: ["req-1"],
    });
    expect(acceptSpy.mock.calls[0][0].acknowledged_reconciled_run_ids).toBeUndefined();
    expect(refresh).toHaveBeenCalled();
    ui.unmount();
  });

  it("전송 직전 최신 snapshot의 후보가 다르면 보내지 않고 갱신 안내를 보인다", async () => {
    const client = installMockClient();
    const { mission } = stage({ integrity: "enforced" });
    const acceptSpy = vi.spyOn(client, "missionAccept");
    const ui = render(mission);
    await flushAsync();
    checkAllConfirmations(ui.container);
    await flushAsync();
    click(q(ui.container, "accept-button")!);
    // 화면은 아직 옛 prop을 들고 있지만 store는 새 후보로 바뀌었다.
    const next = fakeCandidate(mission.id);
    useMissionStore.setState((state) => ({
      missions: { ...state.missions, [mission.id]: { ...mission, candidate_id: next.id } },
    }));
    click(q(ui.container, "accept-confirm-ok")!);
    await flushAsync();
    expect(acceptSpy).not.toHaveBeenCalled();
    expect(q(ui.container, "mission-error-message")?.textContent).toBe(t("missions.error.stale"));
    ui.unmount();
  });

  it("확정 대기가 아니면 상태에 '확정 대기'를 쓰지 않고 버튼 옆에 단계 사유를 보인다", async () => {
    installMockClient();
    const { mission } = stage({ integrity: "enforced", mission: { phase: "reviewing" } });
    const ui = render(mission);
    await flushAsync();
    checkAllConfirmations(ui.container);
    await flushAsync();
    const status = q(ui.container, "result-status")!.textContent ?? "";
    expect(status).toBe(t("missions.result.statusPhase", { phase: t("missions.phase.reviewing") }));
    expect(status).not.toContain("확정 대기");
    expect(q<HTMLButtonElement>(ui.container, "accept-button")!.disabled).toBe(true);
    expect(q(ui.container, "accept-blocked-reason")?.textContent).toBe(
      t("missions.result.issue.notAwaitingAcceptance", { phase: t("missions.phase.reviewing") }),
    );
    const text = ui.container.textContent ?? "";
    expect(text).not.toContain("배포 완료");
    expect(text).not.toContain("main 반영 완료");
    ui.unmount();
  });

  it("미충족 항목의 링크는 할 일·결정 열기로 올린다", async () => {
    installMockClient();
    const { mission } = stage({ integrity: "enforced" });
    const failed = fakeTask(mission.id, "failed", { title: "API 구현" });
    const decision = fakeDecision(mission.id, { blocking: true });
    seedStore({ tasks: [failed], decisions: [decision] });
    const onOpenTask = vi.fn();
    const onOpenDecision = vi.fn();
    const ui = render(mission, { onOpenTask, onOpenDecision });
    await flushAsync();
    expect(q(ui.container, "accept-blocked-reason")?.textContent).toBe(
      t("missions.result.issue.requiredTask", { task: "API 구현", state: t("missions.taskStatus.failed") }),
    );
    const taskIssue = q(ui.container, "checklist-tasks")!;
    click(q(taskIssue, "checklist-link")!);
    expect(onOpenTask).toHaveBeenCalledWith(failed.id);
    click(q(q(ui.container, "checklist-decisions")!, "checklist-link")!);
    expect(onOpenDecision).toHaveBeenCalledWith(decision.id);
    ui.unmount();
  });

  it("요구사항 ✓는 명령별로 현재 후보에서 통과 1건 이상일 때만", async () => {
    installMockClient();
    const { mission, candidate, verification } = stage({ integrity: "enforced" });
    const status = (ui: ReturnType<typeof render>) => q(ui.container, "req-row")?.dataset.status;

    const failedFirst = { ...verification, id: "verification-failed", status: "failed" as const, started_at: "2026-09-13T00:59:00.000Z" };
    seedStore({ verifications: [failedFirst] });
    let ui = render(mission);
    await flushAsync();
    expect(status(ui)).toBe("passed");
    expect(q(ui.container, "req-status")?.textContent).toBe("✓");
    ui.unmount();

    useMissionStore.setState({ verifications: { [failedFirst.id]: failedFirst } });
    ui = render(mission);
    await flushAsync();
    expect(status(ui)).toBe("failed");
    expect(q(ui.container, "req-status")?.textContent).toBe("✗");
    ui.unmount();

    const old = { ...verification, candidate_id: "candidate-old" };
    useMissionStore.setState({ verifications: { [old.id]: old } });
    ui = render(mission);
    await flushAsync();
    expect(status(ui)).toBe("missing");
    expect(candidate.id).not.toBe(old.candidate_id);
    ui.unmount();
  });
});

describe("불확실 실행 확인", () => {
  it.each(["ko", "en"] as const)("%s UUID 대신 할 일 제목·시도·상태로 묻고 확인 없이는 확정하지 않는다", async (language) => {
    useI18nStore.setState({ language });
    const client = installMockClient();
    const { mission } = stage({ integrity: "enforced" });
    const task = fakeTask(mission.id, "succeeded", { title: "로그인 폼 구현", attempt_count: 2 });
    const run = fakeRun(mission.id, task, "unknown", { attempt: 1, reconciliation_ref: fakeRef("30") });
    task.active_run_id = null;
    seedStore({ tasks: [task], runs: [run] });
    const accept = vi.spyOn(client, "missionAccept").mockResolvedValue({ ...MUTATION, mission_id: mission.id });
    const onOpenTask = vi.fn();
    const ui = render(mission, { onOpenTask });
    await flushAsync();
    const review = q(ui.container, "reconciled-accept-review")!;
    expect(review.textContent).toContain(t("missions.result.uncertainReview"));
    const label = review.querySelector("label")!.textContent ?? "";
    expect(label).toContain("로그인 폼 구현");
    expect(label).toContain(t("missions.runState.unknown"));
    expect(label).not.toContain(run.id);
    click(q(review, "reconciled-open-task")!);
    expect(onOpenTask).toHaveBeenCalledWith(task.id);

    click(q(ui.container, "human-check")!);
    await flushAsync();
    const button = q<HTMLButtonElement>(ui.container, "accept-button")!;
    expect(button.disabled).toBe(true);
    click(q(review, "reconciled-check")!);
    await flushAsync();
    expect(button.disabled).toBe(false);
    click(button);
    click(q(ui.container, "accept-confirm-ok")!);
    await flushAsync();
    expect(accept.mock.calls[0][0]).toMatchObject({ acknowledged_reconciled_run_ids: [run.id], candidate_id: mission.candidate_id });
    expect(useMissionStore.getState().runs[run.id]).toEqual(run);
    ui.unmount();
  });

  it("종료 증거가 없으면 선택할 수 없고 이유를 보이며, 증거·후보가 바뀌면 체크가 풀린다", async () => {
    const client = installMockClient();
    const { mission } = stage({ integrity: "enforced" });
    const task = fakeTask(mission.id, "succeeded");
    const run = fakeRun(mission.id, task, "interrupted", { reconciliation_ref: null });
    seedStore({ tasks: [task], runs: [run] });
    const accept = vi.spyOn(client, "missionAccept");
    const ui = render(mission);
    await flushAsync();
    click(q(ui.container, "human-check")!);
    const input = () => q<HTMLInputElement>(ui.container, "reconciled-check")!;
    const button = () => q<HTMLButtonElement>(ui.container, "accept-button")!;
    expect(input().disabled).toBe(true);
    expect(q(ui.container, "reconciled-unended")?.textContent).toBe(
      t("missions.result.confirmRunUnended", { task: task.title, attempt: run.attempt, state: t("missions.runState.interrupted") }),
    );
    expect(button().disabled).toBe(true);

    useMissionStore.setState((state) => ({ runs: { ...state.runs, [run.id]: { ...run, reconciliation_ref: run.context_ref } } }));
    await flushAsync();
    expect(q(ui.container, "reconciled-unended")).toBeNull();
    click(input());
    await flushAsync();
    expect(input().checked).toBe(true);
    expect(button().disabled).toBe(false);

    useMissionStore.setState((state) => ({
      runs: { ...state.runs, [run.id]: { ...run, reconciliation_ref: { ...run.context_ref, sha256: "b".repeat(64) } } },
    }));
    await flushAsync();
    expect(input().checked).toBe(false);
    expect(button().disabled).toBe(true);

    click(input());
    await flushAsync();
    const next = fakeCandidate(mission.id);
    useMissionStore.setState((state) => ({ candidates: { ...state.candidates, [next.id]: next } }));
    ui.rerender(<ResultReview mission={{ ...mission, candidate_id: next.id }} onRefresh={() => undefined} />);
    await flushAsync();
    expect(input().checked).toBe(false);
    expect(button().disabled).toBe(true);
    click(button());
    await flushAsync();
    expect(accept).not.toHaveBeenCalled();
    ui.unmount();
  });
});

describe("accept binding(05 §9)", () => {
  it("후보가 바뀌면 disable + 갱신 요구, 갱신 후 새 후보 기준으로 다시 확인한다", async () => {
    installMockClient();
    const { mission, verifyTask, verification } = stage({ integrity: "enforced" });
    const refresh = vi.fn();
    const ui = render(mission, { onRefresh: refresh });
    await flushAsync();
    checkAllConfirmations(ui.container);
    await flushAsync();
    const accept = () => q<HTMLButtonElement>(ui.container, "accept-button")!;
    expect(accept().disabled).toBe(false);

    const candidateB = fakeCandidate(mission.id, { created_at: "2026-09-13T02:01:00.000Z", commit_oid: "c".repeat(40) });
    const verificationB = { ...verification, id: "verification-b", candidate_id: candidateB.id, task_id: verifyTask.id };
    const missionB = { ...mission, candidate_id: candidateB.id };
    seedStore({ missions: [missionB], candidates: [candidateB], verifications: [verificationB] });
    ui.rerender(<ResultReview mission={missionB} onRefresh={refresh} />);
    await flushAsync();
    expect(accept().disabled).toBe(true);
    expect(q(ui.container, "candidate-changed")?.textContent).toContain("결과가 바뀌었습니다");

    click(q(ui.container, "candidate-changed")!.querySelector("button")!);
    await flushAsync();
    expect(refresh).toHaveBeenCalled();
    expect(q(ui.container, "candidate-changed")).toBeNull();
    // 체크는 새 후보 기준으로 다시 받는다.
    expect(accept().disabled).toBe(true);
    checkAllConfirmations(ui.container);
    await flushAsync();
    expect(accept().disabled).toBe(false);
    ui.unmount();
  });

  it("확정된 결과는 '확정됨'과 가져오기를 강조하고 확정·버리기 버튼을 숨긴다", async () => {
    installMockClient();
    const { mission } = stage({ integrity: "enforced" });
    const accepted = { ...mission, state: "completed" as const, phase: "done" as const, accepted_at: "2026-09-13T03:00:00.000Z" };
    seedStore({ missions: [accepted] });
    const ui = render(accepted);
    await flushAsync();
    expect(q(ui.container, "result-status")?.textContent).toBe(t("missions.result.statusAccepted"));
    expect(q(ui.container, "accept-button")).toBeNull();
    expect(q(ui.container, "discard-button")).toBeNull();
    expect(q(ui.container, "accepted-notice")).not.toBeNull();
    expect(q(ui.container, "import-summary")?.className).toContain("emphasized");
    const details = q(ui.container, "result-details")!;
    expect(details.firstElementChild?.getAttribute("data-anchor")).toBe("import");
    ui.unmount();
  });
});

describe("결과 가져오기", () => {
  function clipboardStub() {
    const writeText = vi.fn(async (_text: string) => undefined);
    Object.defineProperty(navigator, "clipboard", { value: { writeText }, configurable: true });
    return writeText;
  }

  it("OID·참조·명령을 보여 주고 복사한다", async () => {
    installMockClient();
    const writeText = clipboardStub();
    const { mission, candidate } = stage({ integrity: "enforced", mission: { title: "Add login form" } });
    const ui = render(mission);
    await flushAsync();
    expect(q(ui.container, "import-commit")?.textContent).toBe(candidate.commit_oid);
    expect(q(ui.container, "import-base")?.textContent).toBe(candidate.base_oid);
    expect(q(ui.container, "import-ref")?.textContent).toBe(`refs/iyagi/missions/${mission.id}/candidates/${candidate.id}`);
    expect(q(ui.container, "import-command-merge")?.textContent).toContain(`git merge --ff-only ${candidate.commit_oid}`);
    expect(q(ui.container, "import-command-branch")?.textContent).toContain(`git switch -c ai/add-login-form ${candidate.commit_oid}`);
    expect(q(ui.container, "import-command-diff")?.textContent).toContain(`git diff ${candidate.base_oid} ${candidate.commit_oid}`);

    click(q(ui.container, "copy-branch")!);
    await flushAsync();
    expect(writeText).toHaveBeenCalledWith(`git switch -c ai/add-login-form ${candidate.commit_oid}`);
    expect(q(ui.container, "import-command-branch")?.textContent).toContain(t("missions.result.copied"));
    ui.unmount();
  });

  it("현재 HEAD가 base와 다르면 경고하고 새 브랜치를 추천한다", async () => {
    const client = installMockClient();
    clipboardStub();
    const { mission, candidate } = stage({ integrity: "enforced", mission: { title: "Add login form" } });
    const inspect = vi.spyOn(client, "repositoryInspect").mockResolvedValue({
      repository_id: "repo", canonical_path: "/repo", head_oid: "d".repeat(40), clean: true, dirty_paths: [], verification_supported: true,
    });
    let ui = render(mission);
    await flushAsync();
    expect(inspect).toHaveBeenCalledWith({ path: mission.repository_path });
    expect(q(ui.container, "import-summary")?.textContent).toContain(
      "현재 브랜치가 AI 작업 시작 시점과 다릅니다 — ff-only 병합은 실패할 수 있으니 새 브랜치로 가져오세요",
    );
    expect(q(ui.container, "import-summary-command")?.textContent).toBe(`git switch -c ai/add-login-form ${candidate.commit_oid}`);
    ui.unmount();

    inspect.mockResolvedValue({ repository_id: "repo", canonical_path: "/repo", head_oid: candidate.base_oid, clean: true, dirty_paths: [], verification_supported: true });
    ui = render(mission);
    await flushAsync();
    expect(q(ui.container, "import-head-mismatch")).toBeNull();
    expect(q(ui.container, "import-summary-command")?.textContent).toBe(`git merge --ff-only ${candidate.commit_oid}`);
    ui.unmount();
  });

  it("터미널에서 열기: 저장소 경로로 새 터미널 탭을 열고 명령을 복사한다(자동 실행 없음)", async () => {
    installMockClient();
    const writeText = clipboardStub();
    const { mission, candidate } = stage({ integrity: "enforced", mission: { title: "Add login form" } });
    const controller = {
      newTab: vi.fn(() => "tab-new"),
      createFirstPane: vi.fn(() => true),
      toast: vi.fn(),
    };
    const ui = renderUi(
      <ControllerContext.Provider value={controller as unknown as SessionController}>
        <ResultReview mission={mission} onRefresh={() => undefined} />
      </ControllerContext.Provider>,
    );
    await flushAsync();
    click(q(ui.container, "import-command-diff")!.querySelector("input")!);
    click(q(ui.container, "import-open-terminal")!);
    await flushAsync();
    expect(writeText).toHaveBeenCalledWith(`git diff ${candidate.base_oid} ${candidate.commit_oid}`);
    expect(controller.newTab).toHaveBeenCalledTimes(1);
    expect(controller.createFirstPane).toHaveBeenCalledWith("tab-new", mission.repository_path);
    expect(controller.toast).toHaveBeenCalledWith("명령을 복사했습니다. 터미널에 붙여 넣으세요");
    ui.unmount();
  });
});

describe("받아들이지 않을 때", () => {
  it("수정 요청은 Lead 대화로 올린다", async () => {
    installMockClient();
    const { mission } = stage();
    const onRequestChanges = vi.fn();
    const ui = render(mission, { onRequestChanges });
    await flushAsync();
    click(q(ui.container, "request-changes")!);
    expect(onRequestChanges).toHaveBeenCalledTimes(1);
    ui.unmount();
  });

  it("버리기: 확인창 → 중단 → 종료 확인 후 보관", async () => {
    const client = installMockClient();
    const { mission } = stage();
    const control = vi.spyOn(client, "missionControl").mockImplementation(async (params: MissionControlParams) => ({
      ...MUTATION,
      mission_id: params.mission_id,
    }));
    const onArchived = vi.fn();
    const ui = render(mission, { onArchived });
    await flushAsync();
    click(q(ui.container, "discard-button")!);
    const confirm = q(ui.container, "discard-confirm")!;
    expect(q(ui.container, "result-summary")?.contains(confirm)).toBe(true);
    expect(confirm.textContent).toContain("AI 작업을 중단하고 보관합니다. 파일과 작업 공간은 삭제하지 않습니다.");
    expect(control).not.toHaveBeenCalled();

    click(q(confirm, "discard-confirm-ok")!);
    await flushAsync();
    expect(control).toHaveBeenCalledTimes(1);
    expect(control.mock.calls[0][0]).toMatchObject({ mission_id: mission.id, action: "cancel", expected_revision: mission.revision });
    expect(q(ui.container, "discard-status")?.textContent).toBe(t("missions.result.discardStopping"));

    // 데몬이 종료를 확인했다(snapshot 갱신).
    const cancelled = { ...mission, state: "cancelled" as const, revision: "7" };
    seedStore({ missions: [cancelled] });
    ui.rerender(<ResultReview mission={cancelled} onRefresh={() => undefined} onArchived={onArchived} />);
    await flushAsync();
    expect(control).toHaveBeenCalledTimes(2);
    expect(control.mock.calls[1][0]).toMatchObject({ action: "archive", expected_revision: "7" });
    expect(onArchived).toHaveBeenCalledTimes(1);
    expect(q(ui.container, "discard-status")?.textContent).toBe(t("missions.result.discardDone"));
    ui.unmount();
  });
});

describe("변경·검증 내용", () => {
  it("데몬 manifest entries로 파일 수·change 라벨·크기를 보이고 20개 넘으면 '외 N개 파일'", async () => {
    const client = installMockClient();
    const { mission, candidate } = stage({ integrity: "enforced" });
    const entries = [
      { path: "src/login.ts", change: "added", bytes: 1834, sha256: "f".repeat(64) },
      { path: "src/legacy.ts", change: "deleted", bytes: 0, sha256: "" },
      ...Array.from({ length: 21 }, (_, index) => ({ path: `src/file-${index}.ts`, change: "modified", bytes: 10, sha256: "e".repeat(64) })),
    ];
    const manifestRef = await uploadMockText(client, JSON.stringify({ base_oid: candidate.base_oid, entries }));
    seedStore({ candidates: [{ ...candidate, manifest_ref: manifestRef }] });
    const ui = render(mission);
    await flushAsync();
    expect(q(ui.container, "result-diff-summary")?.textContent).toBe(
      t("missions.result.changesSummary", { count: 23, commit: candidate.commit_oid.slice(0, 10) }),
    );
    const files = ui.container.querySelectorAll('[data-testid="result-file"]');
    expect(files).toHaveLength(20);
    expect(files[0].textContent).toBe("추가 src/login.ts · 1.8 KiB");
    expect(files[1].textContent).toBe("삭제 src/legacy.ts");
    expect(files[2].textContent).toBe("수정 src/file-0.ts · 10 B");
    expect(q(ui.container, "result-files-more")?.textContent).toBe("외 3개 파일");
    ui.unmount();
  });

  it("검증은 명령 이름·결과·무결성 노트를, 남은 한계는 심각도 라벨을 보인다", async () => {
    installMockClient();
    const { mission, candidate, reviewRun } = stage();
    useMissionStore.setState({
      findings: {
        "finding-1": {
          id: "finding-1", mission_id: mission.id, candidate_id: candidate.id, reviewer_run_id: reviewRun.id, severity: "minor",
          path: "api.txt", line: 1, evidence_ref: fakeRef(), requirement_id: null, resolution: "open", resolution_ref: null,
        },
      },
    });
    const ui = render(mission);
    await flushAsync();
    const verification = q(ui.container, "result-verification")!;
    expect(verification.textContent).toContain("단위 시험");
    expect(verification.textContent).toContain(t("missions.verification.passed"));
    expect(verification.textContent).toContain(t("missions.integrity.note.observed"));
    expect(q(verification, "verification-log")).not.toBeNull();
    expect(q(ui.container, "result-limitations")?.textContent).toBe("보통 · api.txt");
    expect(q(ui.container, "finding-severity")?.textContent).toBe("보통");
    ui.unmount();
  });
});

/** 사용자가 이 CLI 버전의 실험적 자동 실행에 동의해 열린 연결(계약 A). */
function experimentalBinding() {
  const binding = compatibleBinding();
  binding.capabilities.structured_result = { supported: true, reason_code: "experimental_opt_in" };
  return binding;
}

describe("데몬 새 기능 연결(계약 A·C·D)", () => {
  it("실험적 연결로 실행한 할 일이 있으면 요약 경고와 확정을 막지 않는 정보 행을 보인다", async () => {
    installMockClient();
    const { mission, reviewRun } = stage({ integrity: "enforced" });
    seedStore({ runs: [{ ...reviewRun, binding_snapshot: experimentalBinding() }] });
    const ui = render(mission);
    await flushAsync();
    expect(q(ui.container, "result-experimental-warning")?.textContent).toContain(t("missions.experimentalRun.resultWarning"));
    const info = q(ui.container, "checklist-experimental")!;
    expect(info.textContent).toContain(t("missions.experimentalRun.checklistInfo", { count: 1 }));
    expect(q(q(ui.container, "acceptance-checklist")!, "checklist-experimental")).toBe(info);
    checkAllConfirmations(ui.container);
    await flushAsync();
    expect(q<HTMLButtonElement>(ui.container, "accept-button")?.disabled).toBe(false);
    expect(q(ui.container, "accept-blocked-reason")).toBeNull();
    ui.unmount();
  });

  it("실험적 연결이 없으면 경고와 정보 행을 그리지 않는다", async () => {
    installMockClient();
    const { mission } = stage({ integrity: "enforced" });
    const ui = render(mission);
    await flushAsync();
    expect(q(ui.container, "result-experimental-warning")).toBeNull();
    expect(q(ui.container, "checklist-experimental")).toBeNull();
    ui.unmount();
  });

  it("확정 뒤에만 작업 공간 정리를 제안하고, 확인창을 거쳐 정리 결과를 보인다", async () => {
    const client = installMockClient();
    const { mission } = stage({ integrity: "enforced" });
    const usage = vi.fn(async (params: WorkspaceUsageParams): Promise<WorkspaceUsageResult> => ({
      missions: [{ mission_id: params.mission_id ?? "", workspaces: 3, bytes: String(10 * 1024 * 1024), cleanable: true, blocked_reason: null }],
      total_bytes: String(10 * 1024 * 1024),
    }));
    const cleanup = vi.fn(async (_params: WorkspaceCleanupParams): Promise<WorkspaceCleanupResult> => ({
      removed: 3, freed_bytes: String(10 * 1024 * 1024), kept: [],
    }));
    Object.assign(client, { workspaceUsage: usage, workspaceCleanup: cleanup });

    let ui = render(mission);
    await flushAsync();
    expect(usage).not.toHaveBeenCalled();
    expect(q(ui.container, "workspace-cleanup-button")).toBeNull();
    ui.unmount();

    const accepted = { ...mission, state: "completed" as const, phase: "done" as const, accepted_at: "2026-09-13T03:00:00.000Z" };
    seedStore({ missions: [accepted] });
    ui = render(accepted);
    await flushAsync();
    expect(usage.mock.calls[0][0]).toEqual({ mission_id: mission.id });
    const button = q(q(ui.container, "result-summary")!, "workspace-cleanup-button")!;
    expect(button.textContent).toBe(t("missions.workspaceCleanup.suggest", { size: "10 MB" }));
    click(button);
    expect(q(ui.container, "workspace-cleanup-confirm")?.textContent).toContain(t("missions.workspaceCleanup.confirmBody"));
    expect(cleanup).not.toHaveBeenCalled();
    click(q(ui.container, "workspace-cleanup-ok")!);
    await flushAsync();
    expect(cleanup.mock.calls[0][0]).toEqual({ request_id: expect.any(String), mission_id: mission.id });
    expect(q(ui.container, "workspace-cleanup-result")?.textContent).toContain(
      t("missions.workspaceCleanup.done", { removed: 3, size: "10 MB" }),
    );
    expect(q(ui.container, "workspace-cleanup-kept")).toBeNull();
    ui.unmount();
  });

  it("확정 뒤 사용량 조회가 실패하면 제안을 조용히 숨긴다", async () => {
    const client = installMockClient();
    const { mission } = stage({ integrity: "enforced" });
    Object.assign(client, { workspaceUsage: vi.fn(async () => { throw new Error("unknown method"); }) });
    const accepted = { ...mission, state: "completed" as const, phase: "done" as const, accepted_at: "2026-09-13T03:00:00.000Z" };
    seedStore({ missions: [accepted] });
    const ui = render(accepted);
    await flushAsync();
    expect(q(ui.container, "workspace-cleanup")).toBeNull();
    expect(q(ui.container, "workspace-cleanup-button")).toBeNull();
    expect(ui.container.querySelectorAll("[role=alert]")).toHaveLength(0);
    ui.unmount();
  });

  it("사용자 확인으로 정리된 불확실 실행에는 '직접 확인으로 정리됨 · 외부 영향 미확인' 라벨을 붙인다", async () => {
    installMockClient();
    const { mission } = stage({ integrity: "enforced" });
    const attestedTask = fakeTask(mission.id, "succeeded", { title: "직접 확인한 작업" });
    const observedTask = fakeTask(mission.id, "succeeded", { title: "관측 종료 작업" });
    const attested = fakeRun(mission.id, attestedTask, "interrupted", { reconciliation_ref: fakeRef("300"), reconciliation_kind: "user_attested" });
    const observed = fakeRun(mission.id, observedTask, "unknown", { reconciliation_ref: fakeRef("300"), reconciliation_kind: "exec_exited" });
    attestedTask.active_run_id = null;
    observedTask.active_run_id = null;
    seedStore({ tasks: [attestedTask, observedTask], runs: [attested, observed] });
    const ui = render(mission);
    await flushAsync();
    const rows = [...ui.container.querySelectorAll<HTMLElement>('[data-testid="reconciled-run"]')];
    const rowFor = (title: string) => rows.find((row) => row.textContent?.includes(title))!;
    expect(q(rowFor("직접 확인한 작업"), "run-attested-label")?.textContent).toBe(t("missions.attest.label"));
    expect(q(rowFor("관측 종료 작업"), "run-attested-label")).toBeNull();
    // 사용자 확인으로 정리됐어도 확정 전 검토 체크 대상이다(외부 영향 미확인).
    expect(q<HTMLInputElement>(rowFor("직접 확인한 작업"), "reconciled-check")?.disabled).toBe(false);
    ui.unmount();
  });
});
