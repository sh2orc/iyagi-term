import { compatibleBinding } from "./testSupport";
/**
 * MissionCreate 시험(05 §3): 제출 흐름(goal artifact upload → mission.create
 * → mission.control(start))과 start 실패 재시도(중복 생성 방지·base_changed 재생성),
 * 그리고 단순화한 입력(연 자리의 저장소 자동 확인과 Git 안내, 첫 팀 자동 선택, 빠른 설정,
 * 리뷰 포함/생략, 선택 사항인 완료 조건과 검증 미지원 OS, 분 단위 시간 제한, 후속 작업 base).
 * fake(Mock)DaemonClient 사용.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { Binding } from "../../generated/Binding";
import type { MissionControlParams } from "../../generated/MissionControlParams";
import type { MutationResult } from "../../generated/MutationResult";
import type { Role } from "../../generated/Role";
import type { TeamTemplate } from "../../generated/TeamTemplate";
import { MISSION_TEXT_MAX_BYTES } from "./artifactUpload";
import { missionPolicy } from "./configuration";
import { newRequestId } from "./viewUtils";
import { MissionCreate } from "./MissionCreate";
import { MissionPage } from "./MissionPage";
import { useMissionUiStore } from "./uiStore";
import { startMissionSync, useMissionStore } from "./store";
import {
  click,
  dispatch,
  fakeCandidate,
  fakeMission,
  flushAsync,
  installMockClient,
  renderUi,
  resetAllMissionState,
  seedStore,
  setValue,
} from "./testSupport";
import { RpcClientError } from "../daemon/client";
import { useWorkbenchStore } from "../../store/workbenchStore";
import { t } from "../../i18n";

/** 빠른 설정(QuickSetup)은 자체 시험이 있다 — 여기서는 자리와 콜백만 본다. */
const quickSetupStub = vi.hoisted(() => ({
  compact: [] as Array<boolean | undefined>,
  template: null as TeamTemplate | null,
}));

vi.mock("./QuickSetup", async () => {
  const { createElement } = await import("react");
  return {
    QuickSetup: (props: { compact?: boolean; onTeamCreated?: (template: TeamTemplate) => void }) => {
      quickSetupStub.compact.push(props.compact);
      return createElement("button", {
        type: "button",
        "data-testid": "quick-setup-stub",
        onClick: () => {
          if (quickSetupStub.template) props.onTeamCreated?.(quickSetupStub.template);
        },
      });
    },
  };
});

const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;

beforeEach(() => {
  resetAllMissionState();
  useWorkbenchStore.setState({ tabs: [], activeTabId: null, toast: null, modal: null, page: "terminal" });
  quickSetupStub.compact.length = 0;
  quickSetupStub.template = null;
});

afterEach(() => vi.restoreAllMocks());

function blur(element: Element): void {
  dispatch(element, new FocusEvent("focusout", { bubbles: true }));
}

function query<T extends Element>(handle: ReturnType<typeof renderUi>, testId: string): T {
  return handle.container.querySelector(`[data-testid='${testId}']`) as T;
}

async function saveTeam(client: ReturnType<typeof installMockClient>, label = "Test team") {
  const binding={...compatibleBinding(),label:"Test model",program:"/fixture",model_id:"fixture"};
  await client.bindingSave({request_id:newRequestId(),expected_revision:"0",binding});
  const saved = await client.templateSave({request_id:newRequestId(),expected_revision:"0",template:{id:newRequestId(),revision:"0",label,repository_id:null,
    role_bindings:(["lead","builder","reviewer","integrator"] as const).map(role=>({role,primary_binding_id:binding.id,fallback_binding_ids:[]})),policy:missionPolicy([binding.id])}});
  return saved.template;
}
/** 역할마다 다른 연결을 묶은 팀 — 정책은 템플릿 그대로(리뷰 생략 팀이면 호출부가 바꾼다). */
async function saveMixedTeam(
  client: ReturnType<typeof installMockClient>,
  byRole: Partial<Record<Role, Binding>>,
  patchPolicy: (policy: TeamTemplate["policy"]) => TeamTemplate["policy"] = (policy) => policy,
) {
  const unique = [...new Map(Object.values(byRole).map((binding) => [binding.id, binding])).values()];
  for (const binding of unique) await client.bindingSave({ request_id: newRequestId(), expected_revision: "0", binding });
  const roles = Object.keys(byRole) as Role[];
  const policy = patchPolicy({ ...missionPolicy(unique.map((binding) => binding.id)), allowed_roles: roles });
  const saved = await client.templateSave({ request_id: newRequestId(), expected_revision: "0", template: { id: newRequestId(), revision: "0", label: "Mixed team", repository_id: null,
    role_bindings: roles.map((role) => ({ role, primary_binding_id: byRole[role]!.id, fallback_binding_ids: [] })), policy } });
  return saved.template;
}
async function configuredClient() {
  const client=installMockClient();
  await saveTeam(client);
  return client;
}
async function fillRequired(handle:ReturnType<typeof renderUi>) {
  await flushAsync(6);
  setValue(handle.container.querySelector("[data-testid='create-repo']") as HTMLInputElement,"/fixture");
  setValue(handle.container.querySelector("[data-testid='create-req-0']") as HTMLInputElement,"Required outcome");
  const select=handle.container.querySelector("[data-testid='create-team']") as HTMLSelectElement;
  setValue(select,select.options[1].value);
  await flushAsync(6);
}

async function fillAndSubmit(handle: ReturnType<typeof renderUi>, goal: string) {
  // templateList/bindingList effect가 마친 뒤 노드를 다시 잡는다 — 로딩
  // 문구가 교체되는 사이에 붙잡은 참조가 떨어져 나가지 않게.
  await fillRequired(handle);
  const goalInput = handle.container.querySelector("[data-testid='create-goal']") as HTMLTextAreaElement;
  setValue(goalInput, goal);
  const submit = handle.container.querySelector("[data-testid='create-submit']") as HTMLButtonElement;
  click(submit);
  await flushAsync(60);
}

it("읽기 지원만 있는 팀은 쓰기 역할을 안내하고 작업을 생성하지 않는다", async () => {
  const client = await configuredClient();
  const binding = (await client.bindingList()).bindings[0];
  binding.capabilities.scoped_write.supported = false;
  await client.bindingSave({request_id:newRequestId(), expected_revision:binding.revision, binding});
  const create = vi.spyOn(client, "missionCreate");
  const upload = vi.spyOn(client, "artifactBegin");
  const ui = renderUi(<MissionCreate onClose={()=>{}}/>);
  await fillRequired(ui);
  setValue(ui.container.querySelector("[data-testid=create-goal]")!, "Implement the requested change");
  // integrator는 선택 역할이지만 팀에 있으면 기능은 검사한다. 역할은 slug 원문이 아니라 사람 이름으로 보인다.
  expect(ui.container.querySelectorAll(".mission-binding-missing")).toHaveLength(2);
  const missingText = [...ui.container.querySelectorAll(".mission-binding-missing")].map((node) => node.textContent).join(" ");
  expect(missingText).toContain(t("missions.role.builder"));
  expect(missingText).toContain(t("missions.role.integrator"));
  expect(missingText).not.toContain("builder 역할");
  // 역할 연결이 모자라면 안내 아래에 빠른 설정을 붙인다(대화상자용 compact).
  expect(ui.container.querySelector("[data-testid=create-quick-setup] [data-testid=quick-setup-stub]")).not.toBeNull();
  expect(quickSetupStub.compact[quickSetupStub.compact.length - 1]).toBe(true);
  // 역할 부족 안내가 이미 이유를 말한다 — 버튼 옆 사유는 겹쳐 보이지 않는다.
  expect(ui.container.querySelector("[data-testid=create-submit-reason]")).toBeNull();
  click(ui.container.querySelector("[data-testid=create-submit]")!); await flushAsync();
  expect(create).not.toHaveBeenCalled(); expect(upload).not.toHaveBeenCalled();
  ui.unmount();
});

describe("MissionCreate 제출 흐름", () => {
  it("목표를 올리고 mission을 만들어 시작한다(중복 생성 없음)", async () => {
    const client = await configuredClient();
    const createSpy = vi.fn(client.missionCreate.bind(client));
    const beginSpy = vi.spyOn(client, "artifactBegin");
    const startSpy = vi.fn(client.missionControl.bind(client));
    client.missionCreate = createSpy;
    client.missionControl = startSpy;
    const onClose = vi.fn();

    const handle = renderUi(<MissionCreate onClose={onClose} />);
    await fillAndSubmit(handle, "로그인 기능을 구현해 주세요.");
    await flushAsync();

    expect(createSpy).toHaveBeenCalledTimes(1);
    expect(beginSpy.mock.calls[0][0].mission_id).toBeNull();
    expect(createSpy.mock.calls[0][0].expected_base_oid).toBe("b".repeat(40));
    expect(createSpy.mock.calls[0][0].requirements[0].id).toMatch(UUID);
    expect(createSpy.mock.calls[0][0].policy.max_automatic_starts).toBe(64);
    // Creation opens the tab before startup; the mounted page owns the request.
    expect(startSpy).not.toHaveBeenCalled();
    expect(onClose).toHaveBeenCalledTimes(1);
    expect(useMissionUiStore.getState().getUi(client.missions[0].id).startRequested).toBe(true);
    handle.unmount();
    // MissionPage 마운트 sync는 스토어의 syncClient(store.ts)를 본다 —
    // installMockClient(별도 레지스트리)만으로는 mission이 UI 스토어에 못
    // 들어와 not-found가 렌더링되고 startRequested 이펙트가 돌지 않는다.
    const stopSync = startMissionSync(client);
    const page = renderUi(<MissionPage missionId={client.missions[0].id} />);
    await flushAsync(30);
    expect(startSpy).toHaveBeenCalledTimes(1);
    expect(startSpy.mock.calls[0][0].action).toBe("start");
    expect(client.missions.length).toBe(1);
    expect(client.missions[0].state).toBe("running");
    expect(onClose).toHaveBeenCalled();
    // mission 탭이 열렸는지.
    expect(useWorkbenchStore.getState().tabs.some((tab) => tab.kind === "mission")).toBe(true);
    page.unmount();
    stopSync();
  });

  it("빈 목표는 검증 오류로 막는다", async () => {
    await configuredClient();
    const onClose = vi.fn();
    const handle = renderUi(<MissionCreate onClose={onClose} />);
    await fillAndSubmit(handle, "   ");
    expect(handle.container.querySelector("[data-testid='create-error']")?.textContent).toContain(
      "목표를 입력하세요",
    );
    expect(onClose).not.toHaveBeenCalled();
    handle.unmount();
  });
});

describe("MissionCreate 탭 상한에서 start 실패 → 재시도", () => {
  beforeEach(() => {
    vi.spyOn(useWorkbenchStore.getState(), "openMissionTab").mockReturnValueOnce(null);
  });
  it("create 성공/start 실패는 오류+재시도를 표시하고 재시도는 같은 mission을 시작한다", async () => {
    const client = await configuredClient();
    // 다음 start 1회만 실패한다(harness의 failNextStart와 같은 주입).
    const originalControl = client.missionControl.bind(client);
    let failedOnce = false;
    client.missionControl = async (params) => {
      if (params.action === "start" && !failedOnce) {
        failedOnce = true;
        throw new RpcClientError("INVALID_STATE", "주입된 시작 실패");
      }
      return originalControl(params);
    };
    const onClose = vi.fn();
    const handle = renderUi(<MissionCreate onClose={onClose} />);

    await fillAndSubmit(handle, "결제 리팩터링");
    await flushAsync();

    // create는 성공(1개), start는 실패 → 대화상자 유지 + 오류 + 재시도 버튼.
    expect(client.missions.length).toBe(1);
    expect(client.missions[0].state).toBe("draft");
    expect(onClose).not.toHaveBeenCalled();
    const startError = handle.container.querySelector("[data-testid='start-error']");
    expect(startError?.textContent).toContain("시작하지 못했습니다");
    // 오류 원문은 문장 대신 자세히에만 둔다.
    expect(startError?.querySelector("[data-testid=mission-error-message]")?.textContent).toBe(t("missions.error.code.invalidState"));
    // 주 버튼이 `다시 시작`으로 바뀌어 같은 대화상자에서 새 작업이 또 생기지 않는다.
    expect(handle.container.querySelector("[data-testid='create-submit']")).toBeNull();
    const retry = handle.container.querySelector("[data-testid='create-retry-start']") as HTMLButtonElement;
    expect(retry.textContent).toBe(t("missions.create.retryStart"));

    client.missionControl = originalControl;
    click(retry);
    await flushAsync();

    // 재시도는 새 mission을 만들지 않는다 — 중복 없이 그대로 시작.
    expect(client.missions.length).toBe(1);
    expect(client.missions[0].state).toBe("running");
    expect(onClose).toHaveBeenCalled();
    handle.unmount();
  });

  it("제출 중에는 버튼이 비활성된다", async () => {
    const client = await configuredClient();
    let releaseSubmit: (() => void) | undefined;
    const gate = new Promise<void>((resolve) => {
      releaseSubmit = resolve;
    });
    const originalControl = client.missionControl.bind(client);
    client.missionControl = async (params) => {
      if (params.action === "start") await gate;
      return originalControl(params);
    };
    const handle = renderUi(<MissionCreate onClose={() => undefined} />);
    await fillRequired(handle);
    const goalInput = handle.container.querySelector("[data-testid='create-goal']") as HTMLTextAreaElement;
    setValue(goalInput, "느린 시작");
    const submit = handle.container.querySelector("[data-testid='create-submit']") as HTMLButtonElement;
    click(submit);
    await flushAsync();
    expect(submit.disabled).toBe(true);
    expect(submit.textContent).toContain("만드는 중");
    void releaseSubmit?.();
    await flushAsync();
    expect(submit.disabled).toBe(false);
    handle.unmount();
  });
});

describe("MissionCreate 저장소 확인", () => {
  it("연 자리의 저장소 경로를 채우고 곧바로 확인한다", async () => {
    const client = await configuredClient();
    const inspect = vi.spyOn(client, "repositoryInspect");
    const ui = renderUi(<MissionCreate onClose={() => undefined} initialRepositoryPath="/work/app" />);
    await flushAsync(6);
    expect(query<HTMLInputElement>(ui, "create-repo").value).toBe("/work/app");
    expect(inspect).toHaveBeenCalledTimes(1);
    expect(inspect).toHaveBeenCalledWith({ path: "/work/app" });
    expect(query<HTMLElement>(ui, "create-repo-status").textContent).toContain("/work/app");
    expect(query<HTMLElement>(ui, "create-repo-dirty")).toBeNull();
    ui.unmount();
  });

  it("경로가 없으면 확인하지 않고, 고친 경로는 입력을 벗어날 때 한 번 확인한다", async () => {
    const client = await configuredClient();
    const inspect = vi.spyOn(client, "repositoryInspect");
    const ui = renderUi(<MissionCreate onClose={() => undefined} />);
    await flushAsync(6);
    const repo = query<HTMLInputElement>(ui, "create-repo");
    expect(repo.value).toBe("");
    blur(repo);
    await flushAsync(4);
    expect(inspect).not.toHaveBeenCalled();

    setValue(repo, "/work/next");
    blur(repo);
    await flushAsync(6);
    expect(inspect).toHaveBeenCalledTimes(1);
    expect(inspect).toHaveBeenCalledWith({ path: "/work/next" });
    expect(query<HTMLElement>(ui, "create-repo-status").textContent).toContain("/work/next");

    // 고치지 않고 다시 벗어나면 확인을 되풀이하지 않는다.
    blur(repo);
    await flushAsync(4);
    expect(inspect).toHaveBeenCalledTimes(1);
    ui.unmount();
  });

  it("커밋하지 않은 변경은 입력 아래에 경로(최대 20개)와 함께 계속 경고하고, 포함 확인란은 기본으로 켜져 있다", async () => {
    const client = await configuredClient();
    const dirtyPaths = Array.from({ length: 25 }, (_, index) => `src/file-${index}.ts`);
    vi.spyOn(client, "repositoryInspect").mockResolvedValue({
      repository_id: "repo-dirty",
      canonical_path: "/work/dirty",
      head_oid: "c".repeat(40),
      clean: false,
      dirty_paths: dirtyPaths,
      verification_supported: true,
    });
    const ui = renderUi(<MissionCreate onClose={() => undefined} initialRepositoryPath="/work/dirty" />);
    await flushAsync(6);
    expect(query<HTMLElement>(ui, "create-repo-dirty").textContent).toContain("커밋하지 않은 변경이 있습니다");
    const listed = [...query<HTMLElement>(ui, "create-repo-dirty-paths").querySelectorAll("li")].map((item) => item.textContent);
    expect(listed).toEqual(dirtyPaths.slice(0, 20));
    expect(query<HTMLElement>(ui, "create-repo-dirty-more").textContent).toBe(t("missions.create.repoDirtyMore", { count: 5 }));
    expect(query<HTMLElement>(ui, "create-repo-dirty").textContent).toContain(t("missions.create.repoDirtyUntracked"));
    expect(query<HTMLInputElement>(ui, "create-include-uncommitted").checked).toBe(true);
    ui.unmount();
  });

  it("커밋하지 않은 변경은 기본값(포함 켜짐)으로는 제출을 막지 않고 include_uncommitted와 현재 HEAD를 보낸다", async () => {
    const client = await configuredClient();
    vi.spyOn(client, "repositoryInspect").mockResolvedValue({
      repository_id: "repo-dirty",
      canonical_path: "/work/dirty",
      head_oid: "c".repeat(40),
      clean: false,
      dirty_paths: ["src/app.ts"],
      verification_supported: true,
    });
    const create = vi.spyOn(client, "missionCreate");
    const ui = renderUi(<MissionCreate onClose={() => undefined} initialRepositoryPath="/work/dirty" />);
    await flushAsync(6);
    expect(query<HTMLInputElement>(ui, "create-include-uncommitted").checked).toBe(true);
    setValue(query<HTMLTextAreaElement>(ui, "create-goal"), "변경을 포함해서 시작해 주세요.");
    click(query<HTMLButtonElement>(ui, "create-submit"));
    await flushAsync(20);
    expect(query<HTMLElement>(ui, "create-error")).toBeNull();
    expect(create).toHaveBeenCalledTimes(1);
    expect(create.mock.calls[0][0]).toMatchObject({ expected_base_oid: "c".repeat(40), include_uncommitted: true });
    ui.unmount();
  });

  it("포함 확인란을 끄면 예전처럼 제출을 막고 mission.create를 부르지 않는다", async () => {
    const client = await configuredClient();
    vi.spyOn(client, "repositoryInspect").mockResolvedValue({
      repository_id: "repo-dirty",
      canonical_path: "/work/dirty",
      head_oid: "c".repeat(40),
      clean: false,
      dirty_paths: ["src/app.ts"],
      verification_supported: true,
    });
    const create = vi.spyOn(client, "missionCreate");
    const ui = renderUi(<MissionCreate onClose={() => undefined} initialRepositoryPath="/work/dirty" />);
    await flushAsync(6);
    click(query<HTMLInputElement>(ui, "create-include-uncommitted"));
    expect(query<HTMLInputElement>(ui, "create-include-uncommitted").checked).toBe(false);
    setValue(query<HTMLTextAreaElement>(ui, "create-goal"), "변경을 정리해 주세요.");
    click(query<HTMLButtonElement>(ui, "create-submit"));
    await flushAsync(20);
    expect(query<HTMLElement>(ui, "create-error").textContent).toContain("Git 변경이 남아 있습니다");
    expect(query<HTMLElement>(ui, "create-repo-dirty")).not.toBeNull();
    expect(create).not.toHaveBeenCalled();
    ui.unmount();
  });
});

describe("MissionCreate 팀", () => {
  it("저장된 첫 팀을 골라 두어 바로 제출할 수 있다", async () => {
    const client = installMockClient();
    const first = await saveTeam(client, "First team");
    await saveTeam(client, "Second team");
    const ui = renderUi(<MissionCreate onClose={() => undefined} />);
    await flushAsync(8);
    const select = query<HTMLSelectElement>(ui, "create-team");
    expect(select.value).toBe(first.id);
    expect(query<HTMLButtonElement>(ui, "create-submit").disabled).toBe(false);
    expect(query<HTMLElement>(ui, "create-quick-setup")).toBeNull();
    ui.unmount();
  });

  it("팀이 없으면 빠른 설정을 보이고, 만든 팀을 목록에 넣어 고른다", async () => {
    const client = installMockClient();
    const ui = renderUi(<MissionCreate onClose={() => undefined} />);
    await flushAsync(6);
    expect(query<HTMLElement>(ui, "create-team")).toBeNull();
    const stub = ui.container.querySelector("[data-testid=create-quick-setup] [data-testid=quick-setup-stub]");
    expect(stub).not.toBeNull();
    expect(quickSetupStub.compact[quickSetupStub.compact.length - 1]).toBe(true);
    // 팀이 없어 막힌 제출은 버튼 옆에 이유를 짧게 보인다.
    expect(query<HTMLButtonElement>(ui, "create-submit").disabled).toBe(true);
    const reason = query<HTMLElement>(ui, "create-submit-reason");
    expect(reason.textContent).toBe(t("missions.create.submitReason.noTeam"));
    expect(query<HTMLButtonElement>(ui, "create-submit").getAttribute("aria-describedby")).toBe(reason.id);

    // 빠른 설정이 모델 연결과 네 역할 팀을 저장한 뒤 알린 것처럼.
    const created = await saveTeam(client, "Quick team");
    quickSetupStub.template = created;
    click(stub!);
    await flushAsync(8);
    const select = query<HTMLSelectElement>(ui, "create-team");
    expect(select).not.toBeNull();
    expect(select.value).toBe(created.id);
    expect([...select.options].filter((option) => option.value === created.id)).toHaveLength(1);
    expect(query<HTMLElement>(ui, "create-quick-setup")).toBeNull();
    expect(query<HTMLButtonElement>(ui, "create-submit").disabled).toBe(false);
    expect(query<HTMLElement>(ui, "create-submit-reason")).toBeNull();
    expect(query<HTMLButtonElement>(ui, "create-submit").hasAttribute("aria-describedby")).toBe(false);
    ui.unmount();
  });

  it("역할 점검 중에는 사유를 보이고, 모델 연결 목록 조회 실패는 영역 안 오류와 다시 시도로 복구한다", async () => {
    const client = await configuredClient();
    let rejectList: ((cause: Error) => void) | undefined;
    const list = vi.spyOn(client, "bindingList").mockImplementationOnce(
      () =>
        new Promise<never>((_, reject) => {
          rejectList = reject;
        }),
    );
    const ui = renderUi(<MissionCreate onClose={() => undefined} />);
    await flushAsync(6);
    expect(list).toHaveBeenCalledTimes(1);
    expect(query<HTMLButtonElement>(ui, "create-submit").disabled).toBe(true);
    expect(query<HTMLElement>(ui, "create-submit-reason").textContent).toBe(t("missions.create.submitReason.checking"));
    expect(query<HTMLElement>(ui, "create-binding-error")).toBeNull();

    void rejectList?.(new Error("daemon unavailable"));
    await flushAsync(4);
    const error = query<HTMLElement>(ui, "create-binding-error");
    expect(error).not.toBeNull();
    // 원인은 사람 문장으로, 원문은 자세히에만 둔다.
    expect(error.querySelector("[data-testid=mission-error-message]")?.textContent).toBe(
      t("missions.create.bindingLoadError", { error: t("missions.error.generic") }),
    );
    expect(error.querySelector("pre")?.textContent).toContain("daemon unavailable");
    // 영구 비활성이 아니라 이유가 보이고 다시 시도할 수 있다 — 버튼 옆 사유는 겹쳐 보이지 않는다.
    expect(query<HTMLButtonElement>(ui, "create-submit").disabled).toBe(true);
    expect(query<HTMLElement>(ui, "create-submit-reason")).toBeNull();
    expect(ui.container.querySelectorAll(".mission-binding-missing")).toHaveLength(0);

    click(query<HTMLButtonElement>(ui, "create-binding-retry"));
    await flushAsync(6);
    expect(list).toHaveBeenCalledTimes(2);
    expect(query<HTMLElement>(ui, "create-binding-error")).toBeNull();
    expect(query<HTMLButtonElement>(ui, "create-submit").disabled).toBe(false);
    expect(query<HTMLElement>(ui, "create-submit-reason")).toBeNull();
    ui.unmount();
  });
});

describe("MissionCreate 완료 조건·실행 설정", () => {
  it("완료 조건과 실행 설정은 접혀 있고, 실행 설정 요약에 현재 값을 보인다", async () => {
    await configuredClient();
    const ui = renderUi(<MissionCreate onClose={() => undefined} />);
    await flushAsync(6);
    const requirements = query<HTMLDetailsElement>(ui, "create-requirements");
    const runSettings = query<HTMLDetailsElement>(ui, "create-run-settings");
    expect(requirements.hasAttribute("open")).toBe(false);
    expect(runSettings.hasAttribute("open")).toBe(false);
    expect(requirements.querySelector("summary")?.textContent).toBe("완료 조건 (선택)");
    expect(query<HTMLElement>(ui, "create-goal-hint").textContent).toBe(
      "완료 조건을 비우면 확정할 때 목표 달성 여부를 직접 확인합니다.",
    );
    expect(runSettings.querySelector("summary")?.textContent).toBe("실행 설정 · 병렬 4 · 시도 2 · 60분 · 비용 상한 없음");
    ui.unmount();
  });

  it("완료 조건을 비우면 목표 첫 줄(200자)로 사용자 확인 조건 하나를 보낸다", async () => {
    const client = await configuredClient();
    const create = vi.spyOn(client, "missionCreate");
    const ui = renderUi(<MissionCreate onClose={() => undefined} />);
    await flushAsync(6);
    setValue(query<HTMLInputElement>(ui, "create-repo"), "/fixture");
    setValue(query<HTMLTextAreaElement>(ui, "create-goal"), `  ${"가".repeat(300)}\n둘째 줄은 싣지 않는다`);
    click(query<HTMLButtonElement>(ui, "create-submit"));
    await flushAsync(60);
    expect(query<HTMLElement>(ui, "create-error")).toBeNull();
    expect(create).toHaveBeenCalledTimes(1);
    const requirements = create.mock.calls[0][0].requirements;
    expect(requirements).toHaveLength(1);
    expect(requirements[0]).toMatchObject({
      text: `목표 달성 확인: ${"가".repeat(200)}`,
      verification_ids: [],
      human_check: true,
    });
    expect(requirements[0].id).toMatch(UUID);
    expect(client.missions[0].state).toBe("draft");
    expect(useMissionUiStore.getState().getUi(client.missions[0].id).startRequested).toBe(true);
    ui.unmount();
  });

  it("적은 완료 조건이 있으면 기본 조건을 만들지 않는다", async () => {
    const client = await configuredClient();
    const create = vi.spyOn(client, "missionCreate");
    const ui = renderUi(<MissionCreate onClose={() => undefined} />);
    await fillAndSubmit(ui, "로그인 기능을 구현해 주세요.");
    expect(create).toHaveBeenCalledTimes(1);
    expect(create.mock.calls[0][0].requirements).toEqual([
      expect.objectContaining({ text: "Required outcome", verification_ids: [], human_check: false }),
    ]);
    ui.unmount();
  });

  it("시간 제한은 분으로 받아 ms로 보낸다", async () => {
    const client = await configuredClient();
    const create = vi.spyOn(client, "missionCreate");
    const ui = renderUi(<MissionCreate onClose={() => undefined} />);
    await fillRequired(ui);
    expect(ui.container.textContent).toContain("실행 시간 제한(분)");
    const minutes = query<HTMLInputElement>(ui, "create-time-limit");
    expect(minutes.value).toBe("60");
    setValue(minutes, "30");
    expect(query<HTMLElement>(ui, "create-run-settings-summary").textContent).toContain("30분");
    // 1분 미만·빈 값은 받지 않고, 벗어나면 마지막 값으로 되돌린다.
    setValue(minutes, "0");
    blur(minutes);
    expect(minutes.value).toBe("30");
    setValue(query<HTMLTextAreaElement>(ui, "create-goal"), "느린 테스트 줄이기");
    click(query<HTMLButtonElement>(ui, "create-submit"));
    await flushAsync(60);
    expect(create).toHaveBeenCalledTimes(1);
    expect(create.mock.calls[0][0].policy.run_time_limit_ms).toBe("1800000");
    ui.unmount();
  });

  it("시간 제한은 데몬 상한(120분)으로 자르고 지수 표기를 받지 않는다", async () => {
    const client = await configuredClient();
    const create = vi.spyOn(client, "missionCreate");
    const ui = renderUi(<MissionCreate onClose={() => undefined} />);
    await fillRequired(ui);
    const minutes = query<HTMLInputElement>(ui, "create-time-limit");
    expect(minutes.getAttribute("min")).toBe("1");
    expect(minutes.getAttribute("max")).toBe("120");
    const summary = () => query<HTMLElement>(ui, "create-run-settings-summary").textContent;
    // 상한을 넘는 거대한 값은 입력 중에 곧바로 상한으로 보인다.
    setValue(minutes, "99999999999999999999");
    expect(minutes.value).toBe("120");
    expect(summary()).toContain("120분");
    // 지수 표기는 값으로 받지 않고, 벗어나면 마지막으로 받은 값으로 되돌린다.
    setValue(minutes, "45");
    setValue(minutes, "1e3");
    expect(summary()).toContain("45분");
    blur(minutes);
    expect(minutes.value).toBe("45");
    // 상한 바로 위 값도 상한으로 맞추고, 벗어나도 그대로 상한이다.
    setValue(minutes, "121");
    blur(minutes);
    expect(minutes.value).toBe("120");
    expect(summary()).toContain("120분");
    setValue(query<HTMLTextAreaElement>(ui, "create-goal"), "긴 작업");
    click(query<HTMLButtonElement>(ui, "create-submit"));
    await flushAsync(60);
    expect(create).toHaveBeenCalledTimes(1);
    expect(create.mock.calls[0][0].policy.run_time_limit_ms).toBe("7200000");
    ui.unmount();
  });

  it("비용 상한 형식이 틀리면 실행 설정 요약에 금액 대신 오류를 보인다", async () => {
    await configuredClient();
    const ui = renderUi(<MissionCreate onClose={() => undefined} />);
    await flushAsync(6);
    const cost = query<HTMLInputElement>(ui, "create-cost-cap");
    const summary = () => query<HTMLElement>(ui, "create-run-settings-summary").textContent;
    setValue(cost, "abc");
    expect(summary()).toContain(t("missions.create.costCapInvalidSummary"));
    expect(summary()).not.toContain("$abc");
    setValue(cost, "5.00");
    expect(summary()).toContain(t("missions.create.costCapSummary", { cap: "5.00" }));
    setValue(cost, "");
    expect(summary()).toContain(t("missions.create.costCapNone"));
    ui.unmount();
  });
});

describe("MissionCreate 안내 문구", () => {
  it("제목 아래 부제를 보이고, 목표 바이트 수는 한도의 90%부터만 보인다", async () => {
    await configuredClient();
    const ui = renderUi(<MissionCreate onClose={() => undefined} />);
    await flushAsync(6);
    const subtitle = query<HTMLElement>(ui, "create-subtitle");
    expect(subtitle.textContent).toBe(t("missions.create.subtitle"));
    // 부제는 이제 제목 header(닫기 버튼 포함) 바로 다음 형제다.
    expect(ui.container.querySelector("header.mission-create-heading")?.nextElementSibling).toBe(subtitle);
    const goal = query<HTMLTextAreaElement>(ui, "create-goal");
    setValue(goal, "짧은 목표");
    expect(query<HTMLElement>(ui, "create-goal-bytes")).toBeNull();
    setValue(goal, "a".repeat(Math.floor(MISSION_TEXT_MAX_BYTES * 0.9) - 1));
    expect(query<HTMLElement>(ui, "create-goal-bytes")).toBeNull();
    setValue(goal, "a".repeat(Math.ceil(MISSION_TEXT_MAX_BYTES * 0.9)));
    expect(query<HTMLElement>(ui, "create-goal-bytes").textContent).toContain(MISSION_TEXT_MAX_BYTES.toLocaleString());
    ui.unmount();
  });

  it.each([
    ["git_unavailable", "INVALID_STATE", "missions.error.reason.gitUnavailable", "retry"],
    ["no_commits", "INVALID_ARGUMENT", "missions.error.reason.noCommits", "retry"],
    ["not_a_repository", "INVALID_ARGUMENT", "missions.error.reason.notRepository", null],
  ] as const)("저장소 확인 오류 %s는 사람 문장으로 보이고 가능한 행동만 붙인다", async (reason, code, key, action) => {
    const client = await configuredClient();
    const inspect = vi.spyOn(client, "repositoryInspect")
      .mockRejectedValueOnce(new RpcClientError(code, `git said ${reason}`, false, { reason_code: reason }));
    const ui = renderUi(<MissionCreate onClose={() => undefined} initialRepositoryPath="/work/app" />);
    await flushAsync(6);
    const error = query<HTMLElement>(ui, "create-repo-error");
    expect(error.querySelector("[data-testid=mission-error-message]")?.textContent).toBe(t(key));
    const button = error.querySelector<HTMLButtonElement>("[data-testid=mission-error-action]");
    if (action === null) {
      expect(button).toBeNull();
    } else {
      expect(button?.getAttribute("data-action")).toBe(action);
      click(button!);
      await flushAsync(6);
      expect(inspect).toHaveBeenCalledTimes(2);
      expect(query<HTMLElement>(ui, "create-repo-error")).toBeNull();
      expect(query<HTMLElement>(ui, "create-repo-status").textContent).toContain("/work/app");
    }
    ui.unmount();
  });
});

describe("MissionCreate 시작 실패 뒤 중복 생성 방지", () => {
  it("탭을 못 열면 다시 시작은 같은 작업만 시작하고 설정 이동 시 다시 탭을 연다", async () => {
    vi.spyOn(useWorkbenchStore.getState(), "openMissionTab").mockReturnValueOnce(null);
    const client = await configuredClient();
    const originalControl = client.missionControl.bind(client);
    client.missionControl = async (params: MissionControlParams) => {
      if (params.action === "start") throw new RpcClientError("INVALID_STATE", "still failing");
      return originalControl(params);
    };
    const create = vi.spyOn(client, "missionCreate");
    const onClose = vi.fn();
    const ui = renderUi(<MissionCreate onClose={onClose} />);
    await fillAndSubmit(ui, "중복 없이 시작");
    expect(create).toHaveBeenCalledTimes(1);
    expect(query<HTMLElement>(ui, "create-submit")).toBeNull();
    expect(query<HTMLElement>(ui, "create-submit-reason").textContent).toBe(t("missions.create.submitReason.created"));

    click(query<HTMLButtonElement>(ui, "create-retry-start"));
    await flushAsync(20);
    click(query<HTMLButtonElement>(ui, "create-retry-start"));
    await flushAsync(20);
    expect(create).toHaveBeenCalledTimes(1);
    expect(client.missions).toHaveLength(1);
    expect(onClose).not.toHaveBeenCalled();

    const settings = [...ui.container.querySelectorAll("button")].find((button) => button.textContent === t("missions.settings.manage"))!;
    click(settings);
    await flushAsync(2);
    expect(onClose).toHaveBeenCalledTimes(1);
    const state = useWorkbenchStore.getState();
    expect(state.tabs.some((tab) => tab.kind === "mission" && tab.missionId === client.missions[0].id)).toBe(true);
    expect(state.page).toBe("settings");
    expect(state.toast).toBe(t("missions.create.draftKeptTab"));
    ui.unmount();
  });

  it("만든 작업이 없으면 설정 열기는 탭을 열지 않는다", async () => {
    await configuredClient();
    const onClose = vi.fn();
    const ui = renderUi(<MissionCreate onClose={onClose} />);
    await flushAsync(6);
    click([...ui.container.querySelectorAll("button")].find((button) => button.textContent === t("missions.settings.manage"))!);
    expect(onClose).not.toHaveBeenCalled();
    expect(useWorkbenchStore.getState().page).toBe("settings");
    expect(useWorkbenchStore.getState().tabs).toHaveLength(0);
    expect(useWorkbenchStore.getState().toast).toBeNull();
    ui.unmount();
  });

  it("탭 상한에서 base_changed면 기존 draft를 취소하고 같은 입력으로 새로 만든다", async () => {
    vi.spyOn(useWorkbenchStore.getState(), "openMissionTab").mockReturnValueOnce(null).mockReturnValueOnce(null);
    const client = await configuredClient();
    const originalControl = client.missionControl.bind(client);
    let failed = false;
    const control = vi.fn(async (params: MissionControlParams) => {
      if (params.action === "start" && !failed) {
        failed = true;
        throw new RpcClientError("INVALID_STATE", "repository HEAD changed", false, { reason_code: "base_changed" });
      }
      return originalControl(params);
    });
    client.missionControl = control;
    const inspect = vi.spyOn(client, "repositoryInspect");
    const create = vi.spyOn(client, "missionCreate");
    const onClose = vi.fn();
    const ui = renderUi(<MissionCreate onClose={onClose} />);
    await fillAndSubmit(ui, "결제 리팩터링");
    expect(create).toHaveBeenCalledTimes(1);
    const startError = query<HTMLElement>(ui, "start-error");
    expect(startError.textContent).toContain(t("missions.create.startFailedLead"));
    expect(startError.querySelector("[data-testid=mission-error-message]")?.textContent).toBe(t("missions.error.reason.baseChanged"));
    // 같은 작업을 다시 시작해도 같은 이유로 거절된다 — 주 버튼은 잠그고 다시 만들기를 권한다.
    expect(query<HTMLButtonElement>(ui, "create-retry-start").disabled).toBe(true);
    const recreate = query<HTMLButtonElement>(ui, "create-recreate-head");
    expect(recreate.textContent).toBe(t("missions.create.recreateHead"));
    const firstId = ((await create.mock.results[0].value) as MutationResult).mission_id;
    const inspectsBefore = inspect.mock.calls.length;

    click(recreate);
    await flushAsync(60);
    expect(control).toHaveBeenCalledWith(expect.objectContaining({ action: "cancel", mission_id: firstId }));
    expect(inspect.mock.calls.length).toBeGreaterThan(inspectsBefore);
    expect(create).toHaveBeenCalledTimes(2);
    const [first, second] = create.mock.calls.map(([params]) => params);
    expect(second.title).toBe(first.title);
    expect(second.requirements.map((requirement) => requirement.text)).toEqual(["Required outcome"]);
    expect(second.role_bindings).toEqual(first.role_bindings);
    expect(second.policy).toEqual(first.policy);
    expect(second.request_id).not.toBe(first.request_id);
    expect(client.missions.find((mission) => mission.id === firstId)?.state).toBe("cancelled");
    expect(client.missions.filter((mission) => mission.state === "running")).toHaveLength(1);
    expect(onClose).toHaveBeenCalled();
    ui.unmount();
  });
});

describe("MissionCreate 검증 미지원 OS", () => {
  it("검증 명령 선택을 막고 안내하며, 조건은 사용자 확인을 기본으로 둔다", async () => {
    const client = await configuredClient();
    vi.spyOn(client, "repositoryInspect").mockResolvedValue({
      repository_id: "repo-win",
      canonical_path: "/work/win",
      head_oid: "b".repeat(40),
      clean: true,
      dirty_paths: [],
      verification_supported: false,
    });
    const create = vi.spyOn(client, "missionCreate");
    const ui = renderUi(<MissionCreate onClose={() => undefined} initialRepositoryPath="/work/win" />);
    await flushAsync(8);
    expect(query<HTMLElement>(ui, "create-verification-unsupported").textContent).toBe(t("missions.create.verificationUnsupportedOs"));
    expect(query<HTMLSelectElement>(ui, "create-req-verification-0").disabled).toBe(true);
    expect(query<HTMLInputElement>(ui, "create-req-human-0").checked).toBe(true);
    click(query<HTMLButtonElement>(ui, "create-add-req"));
    expect(query<HTMLInputElement>(ui, "create-req-human-1").checked).toBe(true);
    setValue(query<HTMLInputElement>(ui, "create-req-0"), "로그인이 된다");
    setValue(query<HTMLTextAreaElement>(ui, "create-goal"), "로그인 수정");
    click(query<HTMLButtonElement>(ui, "create-submit"));
    await flushAsync(60);
    expect(create).toHaveBeenCalledTimes(1);
    expect(create.mock.calls[0][0].requirements).toEqual([
      expect.objectContaining({ text: "로그인이 된다", verification_ids: [], human_check: true }),
    ]);
    ui.unmount();
  });

  it("검증을 지원하는 저장소에서는 선택을 막지 않고 사용자 확인을 강제로 켜지 않는다", async () => {
    const client = await configuredClient();
    vi.spyOn(client, "repositoryInspect").mockResolvedValue({
      repository_id: "repo-mac", canonical_path: "/work/mac", head_oid: "b".repeat(40), clean: true, dirty_paths: [], verification_supported: true,
    });
    const ui = renderUi(<MissionCreate onClose={() => undefined} initialRepositoryPath="/work/mac" />);
    await flushAsync(8);
    expect(query<HTMLElement>(ui, "create-verification-unsupported")).toBeNull();
    expect(query<HTMLSelectElement>(ui, "create-req-verification-0").disabled).toBe(false);
    expect(query<HTMLInputElement>(ui, "create-req-human-0").checked).toBe(false);
    ui.unmount();
  });
});

describe("MissionCreate 리뷰 포함/생략(빠른 모드)", () => {
  it("기본은 리뷰 포함 — 생략하면 reviewer 없이 시작할 수 있고 정책·역할 연결에서 reviewer를 뺀다", async () => {
    const client = installMockClient();
    const model = { ...compatibleBinding(), label: "Model", program: "/fixture", model_id: "fixture" };
    const disabled = { ...compatibleBinding(), label: "Disabled reviewer", program: "/fixture", model_id: "off", enabled: false };
    await saveMixedTeam(client, { lead: model, builder: model, reviewer: disabled, integrator: model });
    const create = vi.spyOn(client, "missionCreate");
    const ui = renderUi(<MissionCreate onClose={() => undefined} />);
    await flushAsync(8);
    const review = query<HTMLElement>(ui, "create-review-mode");
    // 리뷰 방식은 실행 설정 안에서 필요할 때 바꾼다.
    expect(review.closest("details")?.dataset.testid).toBe("create-run-settings");
    expect(query<HTMLInputElement>(ui, "create-review-include").checked).toBe(true);
    expect(ui.container.querySelectorAll(".mission-binding-missing")).toHaveLength(1);
    expect(ui.container.querySelector(".mission-binding-missing")?.textContent).toContain(t("missions.role.reviewer"));
    expect(query<HTMLButtonElement>(ui, "create-submit").disabled).toBe(true);

    click(query<HTMLInputElement>(ui, "create-review-skip"));
    await flushAsync(2);
    expect(query<HTMLInputElement>(ui, "create-review-skip").checked).toBe(true);
    expect(ui.container.querySelectorAll(".mission-binding-missing")).toHaveLength(0);
    expect(query<HTMLButtonElement>(ui, "create-submit").disabled).toBe(false);

    setValue(query<HTMLInputElement>(ui, "create-repo"), "/fixture");
    setValue(query<HTMLTextAreaElement>(ui, "create-goal"), "빠르게 고치기");
    click(query<HTMLButtonElement>(ui, "create-submit"));
    await flushAsync(60);
    expect(create).toHaveBeenCalledTimes(1);
    const params = create.mock.calls[0][0];
    expect(params.policy.require_independent_review).toBe(false);
    expect(params.policy.allowed_roles).toEqual(["lead", "builder", "integrator"]);
    expect(params.role_bindings.map((entry) => entry.role)).toEqual(["lead", "builder", "integrator"]);
    ui.unmount();
  });

  it("리뷰 생략 팀은 생략을 기본으로 고르고 lead·builder만 검사한다(Integrator 없음은 정보 한 줄)", async () => {
    const client = installMockClient();
    const model = { ...compatibleBinding(), label: "Model", program: "/fixture", model_id: "fixture" };
    await saveMixedTeam(client, { lead: model, builder: model }, (policy) => ({ ...policy, require_independent_review: false }));
    const create = vi.spyOn(client, "missionCreate");
    const ui = renderUi(<MissionCreate onClose={() => undefined} />);
    await flushAsync(8);
    expect(query<HTMLInputElement>(ui, "create-review-skip").checked).toBe(true);
    expect(ui.container.querySelectorAll(".mission-binding-missing")).toHaveLength(0);
    // Integrator가 없는 팀 — 막지 않고 충돌 때 쓸 수 있는 방법만 알린다.
    expect(query<HTMLElement>(ui, "create-no-integrator").textContent).toBe(t("missions.create.noIntegrator"));
    // 리뷰 포함으로 바꾸면 reviewer만 모자라다 — integrator는 데몬과 같이 선택 역할이다.
    click(query<HTMLInputElement>(ui, "create-review-include"));
    await flushAsync(2);
    expect([...ui.container.querySelectorAll(".mission-binding-missing")].map((node) => node.textContent).join(" ")).toContain(t("missions.role.reviewer"));
    expect(ui.container.querySelectorAll(".mission-binding-missing")).toHaveLength(1);
    click(query<HTMLInputElement>(ui, "create-review-skip"));
    await flushAsync(2);
    setValue(query<HTMLInputElement>(ui, "create-repo"), "/fixture");
    setValue(query<HTMLTextAreaElement>(ui, "create-goal"), "리뷰 없이");
    click(query<HTMLButtonElement>(ui, "create-submit"));
    await flushAsync(60);
    expect(create.mock.calls[0][0].role_bindings.map((entry) => entry.role)).toEqual(["lead", "builder"]);
    expect(create.mock.calls[0][0].policy).toMatchObject({ require_independent_review: false, allowed_roles: ["lead", "builder"] });
    ui.unmount();
  });

  it("실험적 연결이 팀에 있으면 역할 점검 아래에 한 줄로 알린다", async () => {
    const client = installMockClient();
    const experimental = compatibleBinding();
    for (const key of Object.keys(experimental.capabilities) as Array<keyof Binding["capabilities"]>) {
      experimental.capabilities[key] = { supported: true, reason_code: "experimental_opt_in" };
    }
    const model = { ...experimental, label: "Experimental", program: "/fixture", model_id: "fixture", experimental_version: "2.1.300" };
    await saveMixedTeam(client, { lead: model, builder: model, reviewer: model, integrator: model });
    const ui = renderUi(<MissionCreate onClose={() => undefined} />);
    await flushAsync(8);
    expect(query<HTMLElement>(ui, "create-experimental").textContent).toBe(t("missions.create.experimentalIncluded"));
    expect(query<HTMLButtonElement>(ui, "create-submit").disabled).toBe(false);
    ui.unmount();
  });

  it("선택 기능만 동의로 열린 연결은 실험적 연결로 세지 않는다", async () => {
    // 11 §8: 역할이 맡는 할 일에 필요한 기능(structured_result·events·cancel·읽기/쓰기)만 본다.
    const client = installMockClient();
    const optional = compatibleBinding();
    optional.capabilities.steer = { supported: true, reason_code: "experimental_opt_in" };
    optional.capabilities.resume = { supported: true, reason_code: "experimental_opt_in" };
    const model = { ...optional, label: "Optional", program: "/fixture", model_id: "fixture" };
    await saveMixedTeam(client, { lead: model, builder: model, reviewer: model, integrator: model });
    const ui = renderUi(<MissionCreate onClose={() => undefined} />);
    await flushAsync(8);
    expect(query<HTMLElement>(ui, "create-experimental")).toBeNull();
    expect(query<HTMLButtonElement>(ui, "create-submit").disabled).toBe(false);
    ui.unmount();
  });

  it("검증된 연결만 있으면 실험적 연결 안내가 없다", async () => {
    await configuredClient();
    const ui = renderUi(<MissionCreate onClose={() => undefined} />);
    await flushAsync(8);
    expect(query<HTMLElement>(ui, "create-experimental")).toBeNull();
    // 네 역할 팀은 Integrator가 있으므로 충돌 해결 안내가 없다.
    expect(query<HTMLElement>(ui, "create-no-integrator")).toBeNull();
    ui.unmount();
  });
});

describe("MissionCreate 후속 작업(계약 E)", () => {
  it("확정된 이전 작업의 결과 commit을 base로 보이고 follow_up_of와 함께 보낸다 — HEAD·변경 경고는 없다", async () => {
    const client = await configuredClient();
    const previous = fakeMission({ state: "completed", accepted_at: "2026-09-16T00:00:00.000Z", repository_id: "repo-follow", repository_path: "/work/app" });
    const candidate = fakeCandidate(previous.id, { commit_oid: `abc1234${"0".repeat(33)}` });
    previous.candidate_id = candidate.id;
    seedStore({ missions: [previous], candidates: [candidate] });
    vi.spyOn(client, "repositoryInspect").mockResolvedValue({
      repository_id: "repo-follow", canonical_path: "/work/app", head_oid: "c".repeat(40), clean: false, dirty_paths: ["notes.txt"], verification_supported: true,
    });
    // 이 테스트의 대상은 폼이 무엇을 보내는가다. mock의 follow-up 검증은
    // seedStore로는 만족시킬 수 없어(mock 자체 missionStates만 안다 — 계약 자체는
    // mockClient.test.ts가 다룬다) 성공 응답만 대신 주고 보낸 파라미터를 본다.
    const create = vi.spyOn(client, "missionCreate").mockResolvedValue({
      mission_id: "m-follow-up", revision: "1", event_seq: "1", entity_ids: [],
    });
    const ui = renderUi(<MissionCreate onClose={() => undefined} initialRepositoryPath="/work/app" initialGoal="이어서 작업" followUpOf={previous.id} />);
    await flushAsync(8);
    expect(query<HTMLElement>(ui, "create-follow-up-base").textContent).toBe(t("missions.create.followUpBase", { commit: "abc1234" }));
    expect(query<HTMLElement>(ui, "create-repo-dirty")).toBeNull();
    expect(query<HTMLElement>(ui, "create-repo-status").textContent).not.toContain("c".repeat(12));
    click(query<HTMLButtonElement>(ui, "create-submit"));
    await flushAsync(60);
    expect(query<HTMLElement>(ui, "create-error")).toBeNull();
    expect(create).toHaveBeenCalledTimes(1);
    expect(create.mock.calls[0][0]).toMatchObject({ expected_base_oid: candidate.commit_oid, follow_up_of: previous.id, repository_path: "/work/app" });
    // 저장소가 더러워도 후속 작업은 include_uncommitted를 보내지 않는다(데몬이 follow_up_snapshot으로 거절).
    expect(Object.prototype.hasOwnProperty.call(create.mock.calls[0][0], "include_uncommitted")).toBe(false);
    ui.unmount();
  });

  it("store에 없는 이전 작업은 동기화 기록 없이 한 번만 읽어 base를 정한다(전체 동기화 대상으로 만들지 않는다)", async () => {
    const client = await configuredClient();
    const previous = fakeMission({ state: "completed", accepted_at: "2026-09-16T00:00:00.000Z", repository_id: "repo-follow", repository_path: "/work/app" });
    const candidate = fakeCandidate(previous.id, { commit_oid: `def5678${"0".repeat(33)}` });
    previous.candidate_id = candidate.id;
    vi.spyOn(client, "repositoryInspect").mockResolvedValue({
      repository_id: "repo-follow", canonical_path: "/work/app", head_oid: "c".repeat(40), clean: true, dirty_paths: [], verification_supported: true,
    });
    const original = useMissionStore.getState();
    const peek = vi.fn(async () => {
      seedStore({ missions: [previous], candidates: [candidate] });
      return true;
    });
    const sync = vi.fn(async () => undefined);
    useMissionStore.setState({ peekMission: peek, syncMission: sync });
    const ui = renderUi(<MissionCreate onClose={() => undefined} initialRepositoryPath="/work/app" initialGoal="이어서" followUpOf={previous.id} />);
    try {
      await flushAsync(8);
      expect(peek).toHaveBeenCalledWith(previous.id);
      expect(sync).not.toHaveBeenCalledWith(previous.id);
      expect(query<HTMLElement>(ui, "create-follow-up-base").textContent).toBe(t("missions.create.followUpBase", { commit: "def5678" }));
      expect(useMissionStore.getState().sync[previous.id]).toBeUndefined();
    } finally {
      ui.unmount();
      useMissionStore.setState({ peekMission: original.peekMission, syncMission: original.syncMission });
    }
  });

  it("이전 작업이 확정되지 않았으면 현재 HEAD에서 시작한다고 알리고 follow_up_of 키 없이 보낸다", async () => {
    const client = await configuredClient();
    const previous = fakeMission({ state: "failed", repository_path: "/fixture" });
    seedStore({ missions: [previous] });
    const create = vi.spyOn(client, "missionCreate");
    const ui = renderUi(<MissionCreate onClose={() => undefined} initialRepositoryPath="/fixture" initialGoal="다시" followUpOf={previous.id} />);
    await flushAsync(8);
    expect(query<HTMLElement>(ui, "create-follow-up-fallback").textContent).toBe(t("missions.create.followUpNotAccepted"));
    expect(query<HTMLElement>(ui, "create-follow-up-base")).toBeNull();
    click(query<HTMLButtonElement>(ui, "create-submit"));
    await flushAsync(60);
    expect(create.mock.calls[0][0]).toMatchObject({ expected_base_oid: "b".repeat(40) });
    // 구 데몬(deny_unknown_fields) 호환 — 후속 작업이 아니면 키 자체를 보내지 않는다.
    expect(Object.prototype.hasOwnProperty.call(create.mock.calls[0][0], "follow_up_of")).toBe(false);
    ui.unmount();
  });

  it("다른 저장소를 확인하면 이전 결과를 쓰지 않는다", async () => {
    const client = await configuredClient();
    const previous = fakeMission({ state: "completed", accepted_at: "2026-09-16T00:00:00.000Z", repository_id: "repo-follow", repository_path: "/work/app" });
    const candidate = fakeCandidate(previous.id);
    previous.candidate_id = candidate.id;
    seedStore({ missions: [previous], candidates: [candidate] });
    const create = vi.spyOn(client, "missionCreate");
    const ui = renderUi(<MissionCreate onClose={() => undefined} initialRepositoryPath="/work/other" initialGoal="다른 곳" followUpOf={previous.id} />);
    await flushAsync(8);
    expect(query<HTMLElement>(ui, "create-follow-up-other-repo").textContent).toBe(t("missions.create.followUpOtherRepository"));
    click(query<HTMLButtonElement>(ui, "create-submit"));
    await flushAsync(60);
    expect(create.mock.calls[0][0]).toMatchObject({ expected_base_oid: "b".repeat(40) });
    // 구 데몬(deny_unknown_fields) 호환 — 후속 작업이 아니면 키 자체를 보내지 않는다.
    expect(Object.prototype.hasOwnProperty.call(create.mock.calls[0][0], "follow_up_of")).toBe(false);
    ui.unmount();
  });
});
