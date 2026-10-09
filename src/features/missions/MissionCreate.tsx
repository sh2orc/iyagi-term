/**
 * MissionCreate(05-ui §3): `+` 메뉴 `새 AI 작업` 대화상자.
 *
 * 처음 보이는 것은 저장소·목표·팀뿐이다. 저장소는 연 자리(보고 있는 터미널)의
 * 경로로 채워 곧바로 확인하고, 입력을 고친 뒤 벗어나면 다시 확인한다. 팀은 첫
 * 템플릿을 고르고, 팀이 없거나 역할 연결이 모자라면 빠른 설정(QuickSetup)을 붙인다.
 * 리뷰 포함/생략(계약 B)은 접지 않고 보인다 — 생략이면 독립 리뷰를 끄고 reviewer를
 * 역할 연결·허용 역할에서 빼며, 필수 역할도 lead·builder로 준다(리뷰 포함이면 +reviewer).
 * integrator는 데몬과 같이 선택 역할이다 — 팀에 있으면 기능만 검사하고, 없으면 통합 충돌 때
 * 쓸 수 있는 방법이 줄어든다는 정보 한 줄을 팀 영역에 보인다.
 * 완료 조건(요구사항 + 검증 명령 ID + human check)과 실행 설정(병렬도/시도/
 * 시간(분)/비용/네트워크/계획 자동 적용)은 접어 둔다 — 완료 조건을 비우면 확정 때
 * 사용자가 목표 달성을 확인하는 조건 하나를 만들어 보낸다(데몬은 1개 이상 요구).
 * 이 OS에서 검증 명령을 실행할 수 없으면(verification_supported=false) 검증 선택을 막고
 * 사용자 확인을 기본으로 둔다. 보존 안내와 사용법은 맨 아래.
 *
 * 흐름: goal artifact upload → mission.create → 작업 탭 열기 → 탭에서 start.
 * 탭 상한일 때는 아래의 대화상자 내 start/재시도 흐름을 유지한다.
 * create 성공/start 실패는 오류와 함께 주 버튼을 `다시 시작`(같은 mission, 새 request_id)으로
 * 바꿔 새 작업이 또 생기지 않게 한다. 시작 실패가 base_changed(또는 repository_changed)면
 * `현재 HEAD로 다시 만들기`: 입력은 그대로 두고 기존 draft를 가능하면 취소한 뒤 저장소를
 * 다시 확인해 새로 만든다. 제출 중 버튼 disabled.
 *
 * 후속 작업(계약 E, followUpOf): 이전 작업이 확정 완료면 그 확정 결과 commit을 base로
 * 보이고(`expected_base_oid`) `follow_up_of`를 보낸다 — 저장소 HEAD·커밋하지 않은 변경
 * 경고는 보이지 않는다. 확정되지 않았으면 현재 HEAD에서 시작한다고 알린다.
 *
 * 저장소가 더럽고(커밋하지 않은 변경) 후속 작업이 아니면 더 이상 제출을 막지 않는다 —
 * 경고 안에 기본으로 켜진 확인란을 두어, 켜져 있으면(기본값) `include_uncommitted: true`를
 * 보내 그 변경을 비공개 base 스냅샷(데몬이 만드는 `refs/iyagi/missions/<id>/inputs/base`)으로
 * 포함해 시작한다 — 사용자의 작업 디렉터리·index·HEAD·브랜치는 그대로 두고 커밋·스테이지·
 * stash 하지 않는다(추적되지 않는 파일도 포함, .gitignore 대상은 제외). 확인란을 끄면
 * 예전처럼 `missions.create.repoDirty`로 막는다. `include_uncommitted`는 후속 작업일 때는
 * 절대 보내지 않는다(데몬이 `follow_up_snapshot`으로 거절).
 *
 * Lead/Builder(/리뷰 포함이면 Reviewer) binding이 없으면 설정 링크 + 설명 + 빠른 설정만 제공한다 —
 * 임의 모델 대체 금지(05 §3). 오류는 missionError 문장(MissionErrorNotice)으로 보인다.
 */

import { useEffect, useId, useMemo, useRef, useState } from "react";
import type { Binding } from "../../generated/Binding";
import type { MutationResult } from "../../generated/MutationResult";
import type { Requirement } from "../../generated/Requirement";
import type { Role } from "../../generated/Role";
import type { RepositoryInspectResult } from "../../generated/RepositoryInspectResult";
import type { VerificationCommand } from "../../generated/VerificationCommand";
import type { TeamTemplate } from "../../generated/TeamTemplate";
import { MissionGuide } from "./MissionGuide";
import { QuickSetup } from "./QuickSetup";
import { bindingSupportsRole, isExperimentalBinding, roleTaskKind } from "./bindingSupport";
import {
  dollarsToMicros,
  missionRoleBindings,
  policyWithReviewMode,
  requiredMissionRoles,
  RUN_TIME_LIMIT_CEILING_MS,
} from "./configuration";
import {
  MissionErrorNotice,
  missionError,
  withSupportedActions,
  type MissionErrorAction,
  type MissionTranslate,
  type MissionUiError,
} from "./errors";
import { useI18n } from "../../i18n";
import { MAX_MISSION_TABS, useWorkbenchStore } from "../../store/workbenchStore";
import { useMissionStore } from "./store";
import { useMissionUiStore } from "./uiStore";
import { getMissionClient } from "./clientAccess";
import { MISSION_TEXT_MAX_BYTES, uploadTextArtifact } from "./artifactUpload";
import { roleLabel } from "./labels";
import { newRequestId } from "./viewUtils";
import "./createFlow.css";

interface RequirementDraft {
  key: number;
  text: string;
  verificationId: string;
  human: boolean;
}

let requirementKeySeed = 1;

const MINUTE_MS = 60_000;
const DEFAULT_TIME_LIMIT_MS = 60 * MINUTE_MS;
/** 실행 시간 제한(분)의 상한 — 데몬 policy ceiling(defaults.json)을 넘는 값은 생성이 거부된다. */
const MAX_TIME_LIMIT_MINUTES = Math.max(1, Math.floor(RUN_TIME_LIMIT_CEILING_MS / MINUTE_MS));
/** 숫자 입력에서 지수 표기·부호·소수점을 만드는 키. */
const NON_INTEGER_KEYS = new Set(["e", "E", "+", "-", ".", ","]);
/** 목표 바이트 표시를 켜는 비율 — 한도에 가까워졌을 때만 보인다. */
const GOAL_BYTES_WARNING_RATIO = 0.9;
/** 커밋하지 않은 변경 경고에 나열하는 경로 수. */
const DIRTY_PATHS_SHOWN = 20;
/** 시작 실패가 이 사유면 같은 작업을 다시 시작해도 소용없다 — 현재 저장소 상태로 새로 만든다. */
const RECREATE_REASONS: ReadonlySet<string> = new Set(["base_changed", "repository_changed"]);
/** 대화상자가 수행할 수 있는 오류 행동(다시 시도는 주 버튼·확인 버튼이 맡는다). */
const DIALOG_ACTIONS: readonly MissionErrorAction[] = ["open_settings", "change_model"];

/** 분 입력 문자열을 1..상한 정수로 — 숫자만으로 된 값이 아니거나 1 미만이면 null. */
function clampMinutes(text: string): number | null {
  const trimmed = text.trim();
  if (!/^\d+$/.test(trimmed)) return null;
  const minutes = Number(trimmed);
  if (!Number.isFinite(minutes) || minutes < 1) return null;
  return Math.min(MAX_TIME_LIMIT_MINUTES, minutes);
}
/** 기본 완료 조건에 싣는 목표 첫 줄의 최대 글자 수. */
const DEFAULT_REQUIREMENT_GOAL_CHARS = 200;

/** 목표의 첫 줄을 글자(code point) 단위로 자른다. */
function goalHeadline(goal: string, limit: number): string {
  const line = (goal.trim().split(/\r?\n/)[0] ?? "").trim();
  return Array.from(line).slice(0, limit).join("");
}

/** 대화상자가 직접 판단한 입력 오류 — 번역된 문장 그대로 보인다(RPC 오류와 구분). */
class CreateInputError extends Error {}

function inputError(message: string): MissionUiError {
  return { code: null, reasonCode: null, message, detail: null, action: null };
}

function describeError(t: MissionTranslate, cause: unknown): MissionUiError {
  return cause instanceof CreateInputError ? inputError(cause.message) : missionError(t, cause);
}

/** 후속 작업의 시작점(계약 E). */
type FollowUpBase =
  | { kind: "none" }
  | { kind: "loading" }
  | { kind: "base"; missionId: string; repositoryId: string; repositoryPath: string; commitOid: string }
  | { kind: "fallback" };

function sameRepository(base: Extract<FollowUpBase, { kind: "base" }>, info: Pick<RepositoryInspectResult, "repository_id" | "canonical_path">): boolean {
  return info.repository_id === base.repositoryId || info.canonical_path === base.repositoryPath;
}

export interface MissionCreateProps {
  onClose: () => void;
  /** 대화상자를 연 자리의 저장소 경로 — 있으면 입력을 채우고 마운트 때 곧바로 확인한다. */
  initialRepositoryPath?: string | null;
  /** 목표 입력의 초기값(후속 작업 초안·같은 목표로 새 작업). */
  initialGoal?: string | null;
  /** 이 작업의 확정 결과 위에서 시작하는 후속 작업(계약 E). */
  followUpOf?: string | null;
}

export function MissionCreate(props: MissionCreateProps): JSX.Element {
  const { t } = useI18n();
  const workbenchPage = useWorkbenchStore((state) => state.page);
  const initialRepositoryPath = props.initialRepositoryPath?.trim() ?? "";
  const followUpOf = props.followUpOf?.trim() ? props.followUpOf.trim() : null;
  const [repositoryPath, setRepositoryPath] = useState(initialRepositoryPath);
  const [repository, setRepository] = useState<RepositoryInspectResult | null>(null);
  const [locationOpen, setLocationOpen] = useState(!initialRepositoryPath);
  const [repositoryError, setRepositoryError] = useState<MissionUiError | null>(null);
  const [commands, setCommands] = useState<VerificationCommand[]>([]);
  const [checkingRepository, setCheckingRepository] = useState(false);
  /** 저장소가 더러울 때만 쓰는 선택 — 기본은 켜짐(비공개 base 스냅샷으로 포함해 시작). */
  const [includeUncommitted, setIncludeUncommitted] = useState(true);
  const includeUncommittedId = useId();
  /** 마지막 확인 뒤 경로를 고쳤는가 — 입력을 벗어날 때 자동 확인할지 정한다. */
  const pathEdited = useRef(false);
  /** 화면에 반영할 확인 결과 번호 — 더 새 확인이나 경로 수정이 옛 결과를 버린다. */
  const inspectSeq = useRef(0);
  /** 확인(버튼·자동) 번호 — 진행 표시와 오류는 마지막 확인의 것만 남긴다. */
  const checkSeq = useRef(0);
  const [goal, setGoal] = useState(() => props.initialGoal ?? "");
  const [templates, setTemplates] = useState<TeamTemplate[] | null>(null);
  /** 팀 템플릿 조회 실패(연결 없음 포함). */
  const [templateError, setTemplateError] = useState<MissionUiError | null>(null);
  const [templateId, setTemplateId] = useState<string | null>(null);
  /** 빠른 설정이 모델 연결을 새로 저장했거나 사용자가 다시 시도할 때 역할 점검을 다시 돌리는 신호. */
  const [bindingRefresh, setBindingRefresh] = useState(0);
  /** 모델 연결 목록 조회 실패 — 역할 점검을 못 해 제출이 막힌 이유를 보이고 다시 시도하게 한다. */
  const [bindingError, setBindingError] = useState<MissionUiError | null>(null);
  /** 역할 점검에 쓰는 저장된 모델 연결(조회 중이면 null). */
  const [configuredBindings, setConfiguredBindings] = useState<Binding[] | null>(null);
  /** 리뷰 포함(true)/생략(false). null이면 고른 팀을 따른다. */
  const [reviewChoice, setReviewChoice] = useState<boolean | null>(null);
  const submitReasonId = useId();
  const reviewGroupName = useId();
  const [requirements, setRequirements] = useState<RequirementDraft[]>([
    { key: requirementKeySeed++, text: "", verificationId: "", human: false },
  ]);
  const [maxParallel, setMaxParallel] = useState(4);
  const [maxAttempts, setMaxAttempts] = useState(2);
  const [timeLimitMs, setTimeLimitMs] = useState(DEFAULT_TIME_LIMIT_MS);
  /** 분 입력의 편집 중 문자열 — 지우고 다시 쓰는 동안 값을 강제로 되돌리지 않는다. */
  const [timeLimitText, setTimeLimitText] = useState(String(DEFAULT_TIME_LIMIT_MS / MINUTE_MS));
  const [costCap, setCostCap] = useState("");
  const [unknownCost, setUnknownCost] = useState<"block" | "allow_with_notice" | null>(null);
  const [allowNetwork, setAllowNetwork] = useState(false);
  const [autoPlan, setAutoPlan] = useState(true);
  const [submitting, setSubmitting] = useState(false);
  /** 제출 중인 일이 이미 만든 작업의 다시 시작인가(버튼 문구). */
  const [restarting, setRestarting] = useState(false);
  const [validationError, setValidationError] = useState<MissionUiError | null>(null);
  const [startError, setStartError] = useState<MissionUiError | null>(null);
  /** create는 성공했고 start만 남은 상태(재시도 대상) — 이 동안 새 작업을 만들지 않는다. */
  const [createdMission, setCreatedMission] = useState<{ id: string; revision: string; title: string } | null>(null);
  /** 같은 값을 비동기 흐름이 곧바로 읽는다(렌더 전 재제출 방지). */
  const createdRef = useRef<{ id: string; revision: string; title: string } | null>(null);
  const syncMission = useMissionStore((s) => s.syncMission);

  const rememberCreated = (value: { id: string; revision: string; title: string } | null) => {
    createdRef.current = value;
    setCreatedMission(value);
  };

  // ---------------------------------------------------------------- 후속 작업 base

  const followMission = useMissionStore((s) => (followUpOf ? s.missions[followUpOf] ?? null : null));
  const followCandidate = useMissionStore((s) => {
    const mission = followUpOf ? s.missions[followUpOf] : undefined;
    return mission?.candidate_id ? s.candidates[mission.candidate_id] ?? null : null;
  });
  /** store에 이전 작업·확정 결과가 없을 때 한 번 읽기를 마쳤는가. */
  const [followUpSynced, setFollowUpSynced] = useState(false);
  useEffect(() => {
    if (!followUpOf) return;
    let alive = true;
    const state = useMissionStore.getState();
    const mission = state.missions[followUpOf];
    const needsSync = !mission || (mission.state === "completed" && mission.candidate_id !== null && !state.candidates[mission.candidate_id]);
    if (!needsSync) {
      setFollowUpSynced(true);
      return;
    }
    // 이전 작업은 보통 탭이 없다 — 전체 동기화(sync 기록)를 남기면 이후 힌트마다 snapshot을 다시
    // 뽑으므로, 기록 없이 한 번만 읽는다.
    void state
      .peekMission(followUpOf)
      .catch(() => false)
      .finally(() => {
        if (alive) setFollowUpSynced(true);
      });
    return () => {
      alive = false;
    };
  }, [followUpOf]);
  const followUp: FollowUpBase = useMemo(() => {
    if (!followUpOf) return { kind: "none" };
    const accepted =
      followMission !== null &&
      followMission.state === "completed" &&
      followMission.accepted_at !== null &&
      followCandidate !== null &&
      followCandidate.mission_id === followMission.id &&
      followMission.candidate_id === followCandidate.id;
    if (accepted && followMission && followCandidate) {
      return {
        kind: "base",
        missionId: followMission.id,
        repositoryId: followMission.repository_id,
        repositoryPath: followMission.repository_path,
        commitOid: followCandidate.commit_oid,
      };
    }
    return followUpSynced ? { kind: "fallback" } : { kind: "loading" };
  }, [followUpOf, followMission, followCandidate, followUpSynced]);
  /** 확인한 저장소가 이전 작업의 저장소일 때만(또는 아직 확인 전) 이전 결과 위에서 시작한다. */
  const followUpApplies = followUp.kind === "base" && (repository === null || sameRepository(followUp, repository));

  // 템플릿 로드. 아직 고른 팀이 없을 때만 첫 템플릿을 고른다 — 새로 읽어도 사용자의 선택은 그대로.
  useEffect(() => {
    if (workbenchPage !== "terminal") return;
    let alive = true;
    const client = getMissionClient();
    if (!client) {
      setTemplateError(inputError(t("missions.sync.noClient")));
      return;
    }
    void client
      .templateList({ repository_id: null })
      .then((result) => {
        if (!alive) return;
        setTemplates(result.templates);
        setTemplateError(null);
        setTemplateId((current) => current ?? result.templates[0]?.id ?? null);
      })
      .catch((cause) => {
        if (alive) setTemplateError(missionError(t, cause));
      });
    return () => {
      alive = false;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [workbenchPage]);

  const selectedTemplate = useMemo(
    () => templates?.find((template) => template.id === templateId) ?? null,
    [templates, templateId],
  );
  /** 팀이 정한 리뷰 방식 — reviewer 연결과 독립 리뷰 정책이 모두 있어야 리뷰 포함. */
  const templateReviews =
    selectedTemplate === null ||
    (selectedTemplate.policy.require_independent_review !== false &&
      selectedTemplate.role_bindings.some((entry) => entry.role === "reviewer"));
  const requireReview = reviewChoice ?? templateReviews;

  // 저장된 모델 연결 조회 — 역할 점검은 아래에서 리뷰 방식과 함께 계산한다.
  useEffect(() => {
    setConfiguredBindings(null);
    setBindingError(null);
    if (!selectedTemplate) return;
    let alive = true;
    const client = getMissionClient();
    if (!client) return;
    void client
      .bindingList()
      .then((result) => {
        if (alive) setConfiguredBindings(result.bindings);
      })
      .catch((cause) => {
        if (alive) setBindingError(missionError(t, cause));
      });
    return () => {
      alive = false;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [selectedTemplate, bindingRefresh]);

  // 필수 role binding 점검 — 없으면 설정 링크(대체 모델 구성 금지, 05 §3).
  const roleCheck = useMemo(() => {
    const missing: Role[] = [];
    let experimental = false;
    if (!selectedTemplate || !configuredBindings) return { missing, experimental };
    const configured = new Map(configuredBindings.map((binding) => [binding.id, binding]));
    const teamRoles = selectedTemplate.role_bindings.map((entry) => entry.role);
    for (const role of requiredMissionRoles(requireReview, teamRoles)) {
      const entry = selectedTemplate.role_bindings.find((candidate) => candidate.role === role);
      const model = entry ? configured.get(entry.primary_binding_id) : undefined;
      if (!model || !bindingSupportsRole(model, role)) missing.push(role);
      else if (isExperimentalBinding(model, roleTaskKind(role))) experimental = true;
    }
    return { missing, experimental };
  }, [selectedTemplate, configuredBindings, requireReview]);
  const missingRoles = roleCheck.missing;
  /** 선택한 팀에 충돌 해결 담당(Integrator)이 없는가 — 시작은 막지 않고 정보만 보인다. */
  const teamWithoutIntegrator =
    selectedTemplate !== null && !selectedTemplate.role_bindings.some((entry) => entry.role === "integrator");
  const bindingReady = selectedTemplate !== null && configuredBindings !== null && bindingError === null;

  const goalBytes = useMemo(() => new TextEncoder().encode(goal).byteLength, [goal]);
  const verificationSupported = repository?.verification_supported !== false;

  // 이 OS에서 검증 명령을 실행할 수 없는 저장소를 새로 확인하면 조건을 사용자 확인으로 바꿔 둔다.
  const verificationDefaultsFor = useRef<string | null>(null);
  useEffect(() => {
    if (!repository || repository.verification_supported !== false) {
      verificationDefaultsFor.current = null;
      return;
    }
    if (verificationDefaultsFor.current === repository.repository_id) return;
    verificationDefaultsFor.current = repository.repository_id;
    setRequirements((list) => list.map((item) => ({ ...item, verificationId: "", human: true })));
  }, [repository]);

  const inspectRepository = async () => {
    const seq = ++inspectSeq.current;
    const client = getMissionClient();
    if (!client) throw new CreateInputError(t("missions.sync.noClient"));
    const path = repositoryPath.trim();
    if (!path) throw new CreateInputError(t("missions.create.repoRequired"));
    const info = await client.repositoryInspect({ path });
    const list = await client.verificationList({ repository_id: info.repository_id });
    if (seq === inspectSeq.current) {
      setRepository(info);
      setCommands(list.commands);
    }
    return { info, commands: list.commands };
  };
  const checkRepository = async () => {
    const seq = ++checkSeq.current;
    pathEdited.current = false;
    setCheckingRepository(true);
    setRepositoryError(null);
    try {
      await inspectRepository();
    } catch (e) {
      if (seq === checkSeq.current) setRepositoryError(describeError(t, e));
    } finally {
      if (seq === checkSeq.current) setCheckingRepository(false);
    }
  };

  // 연 자리의 저장소가 채워져 있으면 곧바로 확인한다(마운트 때 한 번).
  useEffect(() => {
    if (initialRepositoryPath) void checkRepository();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const onTeamCreated = (template: TeamTemplate) => {
    setTemplates((list) => {
      const current = list ?? [];
      return current.some((candidate) => candidate.id === template.id)
        ? current.map((candidate) => (candidate.id === template.id ? template : candidate))
        : [...current, template];
    });
    setTemplateError(null);
    setTemplateId(template.id);
    // 빠른 설정이 모델 연결도 저장했다 — 같은 팀이 다시 골라져도 역할 점검을 새로 한다.
    setBindingRefresh((count) => count + 1);
  };

  /** 시작까지 마친 작업의 탭을 열고 대화상자를 닫는다. */
  const finishStarted = (mission: { id: string; title: string }) => {
    rememberCreated(null);
    const openMissionTab = useWorkbenchStore.getState().openMissionTab;
    const tabId = openMissionTab(mission.id, mission.title);
    if (tabId !== null) void syncMission(mission.id);
    // 탭 상한으로 못 열었으면 상한 안내를 "만들었습니다"로 덮지 않는다 — 생성됨과 목록에서 여는 길을 한 토스트로.
    useWorkbenchStore
      .getState()
      .setToast(tabId !== null ? t("missions.create.created") : t("missions.create.createdTabLimit", { count: MAX_MISSION_TABS }));
    props.onClose();
  };

  const submit = async () => {
    // 이미 만든 작업이 시작을 기다리면 새로 만들지 않는다(중복 생성 방지).
    if (createdRef.current) return;
    setValidationError(null);
    setStartError(null);
    if (goal.trim().length === 0) {
      setValidationError(inputError(t("missions.create.goalEmpty")));
      return;
    }
    if (goalBytes > MISSION_TEXT_MAX_BYTES) {
      setValidationError(inputError(t("missions.create.goalTooBig")));
      return;
    }
    const client = getMissionClient();
    if (!client) {
      setValidationError(inputError(t("missions.sync.noClient")));
      return;
    }
    setSubmitting(true);
    try {
      if (!selectedTemplate || !bindingReady || missingRoles.length) throw new CreateInputError(t("missions.create.teamRequired"));
      if (followUp.kind === "loading") throw new CreateInputError(t("missions.create.submitReason.followUpChecking"));
      let cost: string | null;
      try { cost = dollarsToMicros(costCap); } catch { throw new CreateInputError(t("missions.create.costInvalid")); }
      // 제출이 저장소를 직접 다시 확인한다 — 앞서 떠 있던 확인(입력을 벗어날 때 등)의 진행·오류 표시는 버린다.
      checkSeq.current += 1;
      setCheckingRepository(false);
      setRepositoryError(null);
      const inspected = await inspectRepository();
      const followUpBase = followUp.kind === "base" && sameRepository(followUp, inspected.info) ? followUp : null;
      // 후속 작업은 이전 결과 commit에서 시작한다 — 저장소 checkout의 변경 여부는 데몬이 판단한다.
      const dirtyBase = !followUpBase && !inspected.info.clean;
      // 확인란을 껐으면 예전처럼 막는다. 켜져 있으면(기본값) 비공개 base 스냅샷으로 포함해 시작한다.
      if (dirtyBase && !includeUncommitted) throw new CreateInputError(t("missions.create.repoDirty"));
      const canVerify = inspected.info.verification_supported !== false;
      const templatePolicy = selectedTemplate.policy;
      const goalRef = await uploadTextArtifact(client, goal);
      const writtenRequirements: Requirement[] = requirements
        .filter((requirement) => requirement.text.trim().length > 0)
        .map((requirement) => ({
          id: newRequestId(),
          text: requirement.text.trim(),
          verification_ids: canVerify && requirement.verificationId.trim().length > 0 ? [requirement.verificationId.trim()] : [],
          human_check: requirement.human,
        }));
      // 완료 조건을 비우면 확정할 때 사용자가 목표 달성을 직접 확인한다.
      const requirementList: Requirement[] =
        writtenRequirements.length > 0
          ? writtenRequirements
          : [
              {
                id: newRequestId(),
                text: t("missions.create.defaultRequirement", {
                  goal: goalHeadline(goal, DEFAULT_REQUIREMENT_GOAL_CHARS),
                }),
                verification_ids: [],
                human_check: true,
              },
            ];
      if (requirementList.some(requirement => requirement.verification_ids.some(id => !inspected.commands.some(command => command.id === id)))) {
        throw new CreateInputError(t("missions.create.verificationInvalid"));
      }
      const title = goal.trim().split(/\r?\n/)[0].slice(0, 48) || t("missions.tabTitle");
      const created: MutationResult = await client.missionCreate({
        request_id: newRequestId(),
        title,
        repository_path: inspected.info.canonical_path,
        expected_base_oid: followUpBase ? followUpBase.commitOid : inspected.info.head_oid,
        goal_ref: goalRef,
        requirements: requirementList,
        policy: policyWithReviewMode(
          {
            max_parallel_runs: maxParallel,
            max_attempts_per_task: maxAttempts,
            max_repair_cycles: templatePolicy?.max_repair_cycles ?? 1,
            max_automatic_starts: templatePolicy?.max_automatic_starts ?? 1,
            active_time_limit_ms: templatePolicy?.active_time_limit_ms ?? "86400000",
            run_time_limit_ms: String(Math.min(MAX_TIME_LIMIT_MINUTES * MINUTE_MS, Math.max(MINUTE_MS, timeLimitMs))),
            max_cost_usd_micros: cost,
            unknown_cost: unknownCost ?? templatePolicy?.unknown_cost ?? "block",
            allow_network: allowNetwork,
            allow_automatic_plan_apply: autoPlan,
            allow_recovery_of_unsent: templatePolicy?.allow_recovery_of_unsent ?? false,
            allowed_binding_ids: templatePolicy?.allowed_binding_ids ?? [],
            allowed_roles: templatePolicy?.allowed_roles ?? [],
            allowed_verification_ids: [...new Set([...templatePolicy.allowed_verification_ids, ...requirementList.flatMap(requirement => requirement.verification_ids)])],
            require_independent_review: templatePolicy?.require_independent_review ?? true,
            require_enforced_verification: templatePolicy?.require_enforced_verification ?? false,
          },
          requireReview,
        ),
        role_bindings: missionRoleBindings(selectedTemplate.role_bindings, requireReview),
        // 후속 작업일 때만 싣는다 — 구 데몬의 MissionCreateParams는 deny_unknown_fields라
        // `follow_up_of: null`만 보내도 모든 생성을 거절한다.
        ...(followUpBase ? { follow_up_of: followUpBase.missionId } : {}),
        // 더러운 저장소에서 확인란을 켰을 때만 싣는다 — 후속 작업에는 절대 보내지 않는다
        // (데몬이 `follow_up_snapshot`으로 거절).
        ...(dirtyBase && includeUncommitted ? { include_uncommitted: true } : {}),
      });
      // create 성공 → start. start 실패는 재시도 가능 상태로 남긴다(05 §3).
      const mission = { id: created.mission_id, revision: created.revision, title };
      rememberCreated(mission);
      const tabId = useWorkbenchStore.getState().openMissionTab(mission.id, mission.title);
      if (tabId !== null) {
        useMissionUiStore.getState().patchUi(mission.id, { startRequested: true });
        void syncMission(mission.id);
        rememberCreated(null);
        props.onClose();
        return;
      }
      // At the tab limit retain the dialog's existing start/retry flow.
      try {
        await client.missionControl({
          request_id: newRequestId(),
          mission_id: created.mission_id,
          expected_revision: created.revision,
          action: "start",
        });
      } catch (cause) {
        setStartError(missionError(t, cause));
        return;
      }
      finishStarted(mission);
    } catch (cause) {
      setValidationError(describeError(t, cause));
    } finally {
      setSubmitting(false);
    }
  };

  /** create는 됐고 start만 실패한 경우 — 같은 mission으로 재시도(05 §3). */
  const retryStart = async () => {
    const mission = createdRef.current;
    if (!mission) return;
    const client = getMissionClient();
    if (!client) {
      setStartError(inputError(t("missions.sync.noClient")));
      return;
    }
    setSubmitting(true);
    setRestarting(true);
    setStartError(null);
    try {
      await client.missionControl({
        request_id: newRequestId(),
        mission_id: mission.id,
        expected_revision: mission.revision,
        action: "start",
      });
      finishStarted(mission);
    } catch (cause) {
      setStartError(missionError(t, cause));
    } finally {
      setSubmitting(false);
      setRestarting(false);
    }
  };

  /**
   * 기준 커밋(또는 저장소 등록)이 바뀌어 시작할 수 없는 작업 — 입력은 그대로 두고, 기존 draft는
   * 가능하면 취소한 뒤 저장소를 다시 확인해 새로 만든다. 취소가 실패해도 draft는 목록에 남을 뿐이다.
   */
  const recreateFromHead = async () => {
    const previous = createdRef.current;
    if (!previous) return;
    const client = getMissionClient();
    setSubmitting(true);
    if (client) {
      try {
        await client.missionControl({
          request_id: newRequestId(),
          mission_id: previous.id,
          expected_revision: previous.revision,
          action: "cancel",
        });
      } catch {
        // 취소하지 못한 draft는 시작되지 않은 채 목록에 남는다.
      }
    }
    rememberCreated(null);
    setStartError(null);
    // submit이 입력 검증에서 멈춰도 버튼이 잠긴 채 남지 않게 — 곧바로 submit이 다시 잠근다.
    setSubmitting(false);
    await submit();
  };

  const openSettings = () => {
    const draft = createdRef.current;
    const workbench = useWorkbenchStore.getState();
    if (draft) {
      props.onClose();
      // 대화상자를 닫으면 만든 작업을 잃는다 — 탭에 열어 두고 어디에 있는지 알린다.
      const tabId = workbench.openMissionTab(draft.id, draft.title);
      if (tabId !== null) void syncMission(draft.id);
      workbench.openSettings("missions");
      useWorkbenchStore
        .getState()
        .setToast(tabId !== null ? t("missions.create.draftKeptTab") : t("missions.create.draftKeptList", { count: MAX_MISSION_TABS }));
      return;
    }
    workbench.openSettings("missions");
  };
  const onErrorAction = (action: MissionErrorAction) => {
    if (action === "open_settings" || action === "change_model") openSettings();
  };

  const quickSetupClient = getMissionClient();
  // 팀이 하나도 없거나 고른 팀의 역할 연결이 모자라면 이 자리에서 바로 팀을 만들게 한다.
  const showQuickSetup =
    quickSetupClient !== null && templateError === null && templates !== null && (templates.length === 0 || missingRoles.length > 0);
  let costSummary: string;
  try {
    costSummary = dollarsToMicros(costCap) === null
      ? t("missions.create.costCapNone")
      : t("missions.create.costCapSummary", { cap: costCap.trim() });
  } catch {
    costSummary = t("missions.create.costCapInvalidSummary");
  }
  const runSettingsSummary = t("missions.create.runSettingsSummary", {
    parallel: maxParallel,
    attempts: maxAttempts,
    minutes: Math.round(timeLimitMs / MINUTE_MS),
    cost: costSummary,
  });
  const startPending = createdMission !== null;
  const recreateNeeded = startError !== null && startError.reasonCode !== null && RECREATE_REASONS.has(startError.reasonCode);
  // 제출이 막힌 이유 — 역할 부족 안내·연결 조회 오류가 이미 보이면 겹쳐 말하지 않는다.
  let submitReason: string | null = null;
  if (startPending) {
    if (!submitting && !recreateNeeded) submitReason = t("missions.create.submitReason.created");
  } else if (!submitting && !bindingError && missingRoles.length === 0) {
    if (!selectedTemplate) {
      if (templates !== null) {
        submitReason = t(templates.length === 0 ? "missions.create.submitReason.noTeam" : "missions.create.submitReason.team");
      }
    } else if (!bindingReady) {
      submitReason = t("missions.create.submitReason.checking");
    } else if (followUp.kind === "loading") {
      submitReason = t("missions.create.submitReason.followUpChecking");
    }
  }
  const commitTimeLimit = (text: string) => {
    const minutes = clampMinutes(text);
    if (minutes === null) return;
    setTimeLimitMs(minutes * MINUTE_MS);
    // 상한을 넘긴 값은 곧바로 상한으로 보인다(보낸 값과 화면이 어긋나지 않게).
    if (String(minutes) !== text.trim()) setTimeLimitText(String(minutes));
  };
  const dirtyPaths = repository?.dirty_paths ?? [];
  const hiddenDirtyPaths = Math.max(0, dirtyPaths.length - DIRTY_PATHS_SHOWN);
  const primaryDisabled = submitting
    || (startPending ? recreateNeeded : !bindingReady || missingRoles.length > 0 || followUp.kind === "loading");

  return (
    <div className="mission-create" data-testid="mission-create" onKeyDown={(event) => {
      // Editing keys belong to the composer, not the focused terminal behind it.
      event.stopPropagation();
      if (event.key === "Escape" && !event.nativeEvent.isComposing) {
        event.preventDefault();
        if (!submitting) props.onClose();
      }
    }}>
      <header className="mission-create-heading">
        <h2>{t("missions.create.title")}</h2>
        <button type="button" className="icon-button" disabled={submitting} onClick={props.onClose} aria-label={t("missions.common.close")}>×</button>
      </header>
      <p className="muted mission-create-subtitle" data-testid="create-subtitle">{t("missions.create.subtitle")}</p>

      <label className="mission-create-field">
        <span>{t("missions.create.taskPrompt")}</span>
        <textarea
          autoFocus
          rows={5}
          value={goal}
          disabled={submitting}
          placeholder={t("missions.create.goalPlaceholder")}
          data-testid="create-goal"
          onChange={(e) => setGoal(e.target.value)}
        />
        {goalBytes >= MISSION_TEXT_MAX_BYTES * GOAL_BYTES_WARNING_RATIO ? (
          <small className="mission-create-goal-bytes" data-testid="create-goal-bytes">
            {goalBytes.toLocaleString()} / {MISSION_TEXT_MAX_BYTES.toLocaleString()} B
          </small>
        ) : null}
      </label>

      <details className="mission-create-section mission-create-location" open={locationOpen} onToggle={(event) => setLocationOpen(event.currentTarget.open)} data-testid="create-location">
        <summary>{t("missions.create.repo")} · {repositoryPath || t("missions.create.chooseFolder")}</summary>
        <label className="mission-create-field">
          <span>{t("missions.create.repo")}</span>
          <input
            type="text"
            value={repositoryPath}
            disabled={submitting}
            data-testid="create-repo"
            onChange={(e) => {
              setRepositoryPath(e.target.value);
              setRepository(null);
              setCommands([]);
              setRepositoryError(null);
              pathEdited.current = true;
              // 고치기 전 경로의 확인 결과·진행 표시는 버린다.
              inspectSeq.current += 1;
              checkSeq.current += 1;
              setCheckingRepository(false);
            }}
            onBlur={() => {
              if (pathEdited.current && repositoryPath.trim() && !submitting) void checkRepository();
            }}
          />
          <small className="muted">{t("missions.create.repoHint")}</small>
        </label>
        <div className="mission-create-repo-actions">
          <button type="button" disabled={submitting || checkingRepository || !repositoryPath.trim()} onClick={() => void checkRepository()}>{t("missions.create.inspect")}</button>
          {checkingRepository ? (
            <span className="muted" role="status" data-testid="create-repo-checking">
              {t("missions.create.inspecting")}
            </span>
          ) : null}
        </div>
        {repository ? (
          <p className="muted mission-create-repo-status" data-testid="create-repo-status">
            {repository.canonical_path}
            {followUpApplies ? null : ` · ${repository.head_oid.slice(0, 12)}`}
            {!followUpApplies && repository.clean ? ` · ${t("missions.create.repoClean")}` : null}
          </p>
        ) : null}
      </details>
      {followUp.kind === "loading" ? (
        <p className="muted mission-create-follow-up" role="status" data-testid="create-follow-up-loading">
          {t("missions.create.submitReason.followUpChecking")}
        </p>
      ) : followUp.kind === "base" ? (
        followUpApplies ? (
          <p className="mission-create-follow-up" data-testid="create-follow-up-base">
            {t("missions.create.followUpBase", { commit: followUp.commitOid.slice(0, 7) })}
          </p>
        ) : (
          <p className="mission-create-warning" role="status" data-testid="create-follow-up-other-repo">
            {t("missions.create.followUpOtherRepository")}
          </p>
        )
      ) : followUp.kind === "fallback" ? (
        <p className="mission-create-warning" role="status" data-testid="create-follow-up-fallback">
          {t("missions.create.followUpNotAccepted")}
        </p>
      ) : null}
      {repository && !repository.clean && !followUpApplies ? (
        <div className="mission-create-warning mission-create-dirty" role="status" data-testid="create-repo-dirty">
          <p>{t("missions.create.repoDirtyWarning")}</p>
          <details>
            <summary>{t("missions.create.changedFiles", { count: dirtyPaths.length })}</summary>
          {dirtyPaths.length > 0 ? (
            <ul className="mission-create-dirty-paths" data-testid="create-repo-dirty-paths">
              {dirtyPaths.slice(0, DIRTY_PATHS_SHOWN).map((path) => (
                <li key={path}>
                  <code>{path}</code>
                </li>
              ))}
            </ul>
          ) : null}
          {hiddenDirtyPaths > 0 ? <p data-testid="create-repo-dirty-more">{t("missions.create.repoDirtyMore", { count: hiddenDirtyPaths })}</p> : null}
          <p>{t("missions.create.repoDirtyUntracked")}</p>
          </details>
          <label className="mission-create-check" htmlFor={includeUncommittedId}>
            <input
              id={includeUncommittedId}
              type="checkbox"
              checked={includeUncommitted}
              disabled={submitting}
              data-testid="create-include-uncommitted"
              onChange={(e) => setIncludeUncommitted(e.target.checked)}
            />
            {t("missions.create.includeUncommitted")}
          </label>
          <p className="muted">{t("missions.create.includeUncommittedHint")}</p>
        </div>
      ) : null}
      {repositoryError ? (
        <div className="mission-create-notice" data-testid="create-repo-error">
          <MissionErrorNotice
            error={withSupportedActions(repositoryError, ["retry", ...DIALOG_ACTIONS])}
            onRetry={() => void checkRepository()}
            onAction={onErrorAction}
          />
        </div>
      ) : null}

      <div className="mission-create-field">
        <span>{t("missions.create.team")}</span>
        {templates === null && templateError === null ? (
          <p className="muted">{t("missions.create.teamLoading")}</p>
        ) : templateError !== null ? (
          <div className="mission-create-notice" data-testid="create-team-error">
            <MissionErrorNotice
              error={{ ...withSupportedActions(templateError, []), message: `${t("missions.create.teamLoadError")} ${templateError.message}` }}
            />
          </div>
        ) : templates !== null && templates.length === 0 ? (
          <p className="muted">{t("missions.create.teamEmpty")}</p>
        ) : (
          <select
            aria-label={t("missions.create.team")}
            value={templateId ?? ""}
            disabled={submitting}
            data-testid="create-team"
            onChange={(e) => setTemplateId(e.target.value || null)}
          >
            <option value="">—</option>
            {templates?.map((template) => (
              <option key={template.id} value={template.id}>
                {template.label}
              </option>
            ))}
          </select>
        )}
        {missingRoles.map((role) => (
          <p key={role} className="mission-binding-missing">
            {t("missions.create.bindingMissing", { role: roleLabel(t, role) })}
            <button type="button" className="link" onClick={openSettings}>
              {t("missions.create.openSettings")}
            </button>
          </p>
        ))}
        <details className="mission-create-team-help">
          <summary>{t("missions.create.teamOptions")}</summary>
        <button type="button" className="link" onClick={openSettings}>{t("missions.settings.manage")}</button>

        {teamWithoutIntegrator ? (
          <p className="muted mission-create-no-integrator" data-testid="create-no-integrator">
            {t("missions.create.noIntegrator")}
          </p>
        ) : null}
        </details>
        {roleCheck.experimental && missingRoles.length === 0 ? (
          <p className="mission-create-experimental" data-testid="create-experimental">
            {t("missions.create.experimentalIncluded")}
          </p>
        ) : null}
        {bindingError ? (
          <div className="mission-create-notice" data-testid="create-binding-error">
            <MissionErrorNotice
              error={{ ...withSupportedActions(bindingError, []), message: t("missions.create.bindingLoadError", { error: bindingError.message }) }}
            />
            <button
              type="button"
              disabled={submitting}
              onClick={() => setBindingRefresh((count) => count + 1)}
              data-testid="create-binding-retry"
            >
              {t("missions.create.bindingRetry")}
            </button>
          </div>
        ) : null}
        {showQuickSetup && quickSetupClient ? (
          <div className="mission-create-quick-setup" data-testid="create-quick-setup">
            <QuickSetup client={quickSetupClient} compact onTeamCreated={onTeamCreated} />
          </div>
        ) : null}
      </div>

      <details className="mission-create-section" data-testid="create-requirements">
        <summary>
          {t("missions.create.requirements")}{" "}
          <span className="muted mission-create-section-values">{t("missions.create.optional")}</span>
        </summary>
        <p className="muted" data-testid="create-goal-hint">{t("missions.create.goalRequirementHint")}</p>
        <div className="mission-create-field">
          {!verificationSupported ? (
            <p className="mission-create-warning" role="status" data-testid="create-verification-unsupported">
              {t("missions.create.verificationUnsupportedOs")}
            </p>
          ) : null}
          {requirements.map((requirement, index) => (
            <div key={requirement.key} className="mission-requirement-row">
              <input
                type="text"
                className="mission-requirement-text"
                placeholder={t("missions.create.requirementText")}
                value={requirement.text}
                disabled={submitting}
                aria-label={`${t("missions.create.requirementText")} ${index + 1}`}
                data-testid={`create-req-${index}`}
                onChange={(e) =>
                  setRequirements((list) =>
                    list.map((item) => (item.key === requirement.key ? { ...item, text: e.target.value } : item)),
                  )
                }
              />
              <select
                className="mission-requirement-verification"
                value={verificationSupported ? requirement.verificationId : ""}
                disabled={submitting || !repository || !verificationSupported}
                aria-label={`${t("missions.create.requirementVerification")} ${index + 1}`}
                data-testid={`create-req-verification-${index}`}
                onChange={(e) => setRequirements(list => list.map(item => item.key === requirement.key ? {...item, verificationId: e.target.value} : item))}
              >
                <option value="">{t("missions.create.noVerification")}</option>
                {verificationSupported ? commands.map(command => <option key={command.id} value={command.id}>{command.title}</option>) : null}
              </select>
              <label className="mission-requirement-human">
                <input
                  type="checkbox"
                  checked={requirement.human}
                  disabled={submitting}
                  data-testid={`create-req-human-${index}`}
                  onChange={(e) =>
                    setRequirements((list) =>
                      list.map((item) => (item.key === requirement.key ? { ...item, human: e.target.checked } : item)),
                    )
                  }
                />
                {t("missions.create.requirementHuman")}
              </label>
            </div>
          ))}
          <button
            type="button"
            disabled={submitting}
            onClick={() =>
              setRequirements((list) => [
                ...list,
                { key: requirementKeySeed++, text: "", verificationId: "", human: !verificationSupported },
              ])
            }
            data-testid="create-add-req"
          >
            {t("missions.create.addRequirement")}
          </button>
        </div>
      </details>

      <details className="mission-create-section" data-testid="create-run-settings">
        <summary>
          {t("missions.create.runSettings")}
          <span className="muted mission-create-section-values" data-testid="create-run-settings-summary">
            {" · "}
            {runSettingsSummary}
          </span>
        </summary>
        <fieldset className="mission-create-field mission-create-review" data-testid="create-review-mode">
          <legend>{t("missions.create.reviewMode")}</legend>
          <label className="mission-create-check">
            <input
              type="radio"
              name={reviewGroupName}
              checked={requireReview}
              disabled={submitting}
              data-testid="create-review-include"
              onChange={() => setReviewChoice(true)}
            />
            {t("missions.create.reviewInclude")}
          </label>
          <label className="mission-create-check">
            <input
              type="radio"
              name={reviewGroupName}
              checked={!requireReview}
              disabled={submitting}
              data-testid="create-review-skip"
              onChange={() => setReviewChoice(false)}
            />
            {t("missions.create.reviewSkip")}
          </label>
        </fieldset>

        <div className="mission-create-field">
          <label>
            <span>{t("missions.create.parallel")}</span>
            <input
              type="number"
              min={1}
              max={4}
              value={maxParallel}
              disabled={submitting}
              data-testid="create-parallel"
              onChange={(e) => setMaxParallel(Math.min(4, Math.max(1, Math.trunc(Number(e.target.value)) || 1)))}
            />
          </label>
          <label>
            <span>{t("missions.create.attempts")}</span>
            <input
              type="number"
              min={1}
              max={10}
              value={maxAttempts}
              disabled={submitting}
              onChange={(e) => setMaxAttempts(Math.max(1, Number(e.target.value) || 1))}
            />
          </label>
          <label>
            <span>{t("missions.create.timeLimitMinutes")}</span>
            <input
              type="number"
              min={1}
              max={MAX_TIME_LIMIT_MINUTES}
              step={1}
              inputMode="numeric"
              value={timeLimitText}
              disabled={submitting}
              data-testid="create-time-limit"
              onKeyDown={(e) => {
                if (NON_INTEGER_KEYS.has(e.key)) e.preventDefault();
              }}
              onChange={(e) => {
                setTimeLimitText(e.target.value);
                commitTimeLimit(e.target.value);
              }}
              onBlur={() => {
                // 1 미만·빈 값·지수 표기 등은 마지막으로 받은 값으로 되돌린다.
                const minutes = clampMinutes(timeLimitText) ?? Math.round(timeLimitMs / MINUTE_MS);
                setTimeLimitMs(minutes * MINUTE_MS);
                setTimeLimitText(String(minutes));
              }}
            />
          </label>
          <label>
            <span>{t("missions.create.costCap")}</span>
            <input
              type="text"
              value={costCap}
              disabled={submitting}
              placeholder="5.00"
              data-testid="create-cost-cap"
              onChange={(e) => setCostCap(e.target.value)}
            />
          </label>
          <label>
            <span>{t("missions.cost.unknownPolicy")}</span>
            <select value={unknownCost ?? selectedTemplate?.policy.unknown_cost ?? "block"} disabled={submitting} onChange={e=>setUnknownCost(e.target.value as "block" | "allow_with_notice")}>
              <option value="block">{t("missions.cost.blockUnknown")}</option>
              <option value="allow_with_notice">{t("missions.cost.allowUnknown")}</option>
            </select>
          </label>
          <p className="muted">{t("missions.cost.admissionNote")}</p>
          <label className="mission-create-check">
            <input type="checkbox" checked={allowNetwork} disabled={submitting} onChange={(e) => setAllowNetwork(e.target.checked)} />
            {t("missions.create.network")}
          </label>
          <label className="mission-create-check">
            <input type="checkbox" checked={autoPlan} disabled={submitting} onChange={(e) => setAutoPlan(e.target.checked)} />
            {t("missions.create.autoPlan")}
          </label>
        </div>
      </details>

      <details className="mission-create-section">
        <summary>{t("missions.create.help")}</summary>
      <div className="mission-create-field mission-retention">
        <span>{t("missions.create.retention")}</span>
        <p className="muted">{t("missions.create.retentionBody")}</p>
      </div>

      <MissionGuide />

      </details>

      {validationError ? (
        <div className="mission-create-notice" data-testid="create-error">
          <MissionErrorNotice error={withSupportedActions(validationError, DIALOG_ACTIONS)} onAction={onErrorAction} />
        </div>
      ) : null}
      {startError ? (
        <div className="mission-create-notice mission-create-start-error" data-testid="start-error">
          <p className="mission-create-start-lead">{t("missions.create.startFailedLead")}</p>
          <MissionErrorNotice error={withSupportedActions(startError, DIALOG_ACTIONS)} onAction={onErrorAction} />
          {recreateNeeded ? (
            <button type="button" disabled={submitting} onClick={() => void recreateFromHead()} data-testid="create-recreate-head">
              {t(startError.reasonCode === "repository_changed" ? "missions.create.recreateRepository" : "missions.create.recreateHead")}
            </button>
          ) : null}
        </div>
      ) : null}

      {submitReason ? (
        <p className="muted mission-create-submit-reason" id={submitReasonId} data-testid="create-submit-reason">
          {submitReason}
        </p>
      ) : null}
      <div className="mission-create-actions">
        <button
          type="button"
          className="primary"
          disabled={primaryDisabled}
          aria-describedby={submitReason ? submitReasonId : undefined}
          onClick={() => void (startPending ? retryStart() : submit())}
          data-testid={startPending ? "create-retry-start" : "create-submit"}
        >
          {submitting
            ? t(restarting ? "missions.create.starting" : "missions.create.creating")
            : t(startPending ? "missions.create.retryStart" : "missions.create.startTask")}
        </button>
        <button type="button" disabled={submitting} onClick={props.onClose}>
          {t("missions.common.cancel")}
        </button>
      </div>
    </div>
  );
}
