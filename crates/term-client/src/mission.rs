//! O1 mission 클라이언트 표면(T16 + 패리티 확장).
//!
//! mission RPC는 R1과 다른 오류 어휘(MissionErrorCode)를 쓴다
//! (01-contracts O1 §7 — 같은 JSON 봉투, 다른 code 집합). 이 모듈은
//! TUI의 mission 화면이 쓰는 최소 읽기·결정 경로만 typed로 제공한다:
//!
//! * `mission.list` — 목록(페이지).
//! * `mission.snapshot` — 상세(최신 스냅샷).
//! * `mission.control` — 시작/일시정지/재개/취소(결과 확정 흐름의 취소).
//! * `mission.decision.answer` — 결정 옵션 확정.
//! * `repository.inspect` — 미션 생성 플로우 첫 단계(저장소 검증).
//! * `artifact_upload_text` — begin→청크 write→commit(목표·메시지 본문의
//!   ArtifactRef 공급 경로).
//! * `mission.create` — 미션 생성(결과는 원시 값 투영).
//! * `mission_send_message` — 미션 대화 전송(본문 업로드 + mission.message).
//!
//! mutation은 expected_revision CAS다 — 오류 원문을 그대로 돌려 UI가
//! 안내하게 한다. 오류는 R1 RpcError로 정규화한다(code 문자열 보존).

use term_contracts::mission::rpc::{
    methods, ArtifactBeginParams, ArtifactBeginResult, ArtifactCommitParams, ArtifactReadParams,
    ArtifactReadResult, ArtifactWriteParams, ArtifactWriteResult, BindingListResult,
    BindingProbeParams, BindingProbeResult, BindingSaveParams, MissionAcceptParams,
    MissionControlAction, MissionControlParams, MissionCreateParams, MissionDecisionAnswerParams,
    MissionListParams, MissionListResult, MissionMessageParams, RepositoryInspectParams,
    RepositoryInspectResult, TaskControlAction, MissionTaskControlParams,
    MissionActivityParams, MissionActivityResult, TemplateListParams,
    TemplateListResult, TemplateSaveParams, VerificationListParams, VerificationListResult,
    VerificationSaveParams, WorkspaceCleanupParams, WorkspaceCleanupResult, WorkspaceUsageParams,
    WorkspaceUsageResult,
};
use term_contracts::mission::types::{ArtifactRef, Id, Mission};

use crate::client::from_result;
use crate::connection::BridgeError;

/// 목록에서 쓰는 Mission의 필드 투영(전체 타입은 계약 그대로 재사용).
pub type MissionSummary = Mission;

impl crate::client::Client {
    /// `mission.list`(페이지 cursor). 구 데몬은 메서드가 없다 —
    /// 오류로 돌아가고 호출부가 안내한다.
    pub async fn mission_list(
        &self,
        cursor: Option<String>,
        limit: u32,
        archived: bool,
    ) -> Result<MissionListResult, BridgeError> {
        let params = serde_json::to_value(MissionListParams {
            cursor,
            limit,
            archived,
        })
        .expect("mission list params serialize");
        let value = self
            .control
            .call("mlist", methods::MISSION_LIST, params)
            .await?;
        from_result(value, "mission.list")
    }

    /// `mission.snapshot`(최신 스냅샷 — 상세 화면). 결과는 daemon의
    /// SnapshotPage 투영이다(원시 값으로 돌려 UI가 필요한 필드만 읽게
    /// 한다 — 계약 타입 전체를 다시 정의하지 않는다).
    pub async fn mission_snapshot(
        &self,
        mission_id: &str,
    ) -> Result<serde_json::Value, BridgeError> {
        let id: Id = mission_id.parse().map_err(|_| {
            BridgeError::invalid_argument("mission_id must be the mission Id string")
        })?;
        let params = serde_json::json!({ "mission_id": id, "snapshot_id": null });
        self.control
            .call("msnap", methods::MISSION_SNAPSHOT, params)
            .await
    }

    /// `mission.snapshot`에서 열린 결정만 뽑는다(상세 화면의 답변 경로).
    /// 페이지 커서는 생략한다 — 열린 결정은 첫 페이지에 몰려 있다(엔진가
    /// blocking 결정을 먼저 내보낸다).
    pub async fn mission_open_decisions(
        &self,
        mission_id: &str,
    ) -> Result<Vec<term_contracts::mission::types::Decision>, BridgeError> {
        use term_contracts::mission::types::{DecisionState, Entity};
        let value = self.mission_snapshot(mission_id).await?;
        let page: term_contracts::mission::types::SnapshotPage = serde_json::from_value(value)
            .map_err(|e| {
                BridgeError::invalid_argument(format!("mission snapshot decode failed: {e}"))
            })?;
        Ok(page
            .entities
            .into_iter()
            .filter_map(|entity| match entity {
                Entity::Decision(decision) => Some(*decision),
                _ => None,
            })
            .filter(|decision| decision.state == DecisionState::Open)
            .collect())
    }

    /// `mission.snapshot`에서 열린 찾음(finding)만 뽑는다(상세 화면 표시·
    /// 기각 경로 — mission_open_decisions와 같은 한 페이지 정책).
    pub async fn mission_open_findings(
        &self,
        mission_id: &str,
    ) -> Result<Vec<term_contracts::mission::types::Finding>, BridgeError> {
        use term_contracts::mission::types::{Entity, FindingResolution};
        let value = self.mission_snapshot(mission_id).await?;
        let page: term_contracts::mission::types::SnapshotPage = serde_json::from_value(value)
            .map_err(|e| {
                BridgeError::invalid_argument(format!("mission snapshot decode failed: {e}"))
            })?;
        Ok(page
            .entities
            .into_iter()
            .filter_map(|entity| match entity {
                Entity::Finding(finding) => Some(*finding),
                _ => None,
            })
            .filter(|finding| finding.resolution == FindingResolution::Open)
            .collect())
    }

    /// `mission.events`(활동 꼬리 — GUI와 동일 정책: after_seq부터 오름차순,
    /// 페이지 정책은 호출부가 가진다).
    pub async fn mission_events(
        &self,
        mission_id: &str,
        after_seq: u64,
        limit: u32,
    ) -> Result<term_contracts::mission::rpc::MissionEventsResult, BridgeError> {
        let params = serde_json::to_value(term_contracts::mission::rpc::MissionEventsParams {
            mission_id: mission_id.parse().map_err(|_| {
                BridgeError::invalid_argument("mission_id must be the mission Id string")
            })?,
            after_seq: term_contracts::U64String::new(after_seq)
                .expect("seq fits the contract range"),
            limit,
        })
        .expect("mission events params serialize");
        let value = self
            .control
            .call("mevt", methods::MISSION_EVENTS, params)
            .await?;
        from_result(value, "mission.events")
    }

    /// `mission.control`(start/pause/resume/cancel — CAS expected_revision).
    pub async fn mission_control(
        &self,
        request_id: &Id,
        mission_id: &str,
        expected_revision: u64,
        action: MissionControlAction,
    ) -> Result<serde_json::Value, BridgeError> {
        let params = serde_json::to_value(MissionControlParams {
            request_id: request_id.clone(),
            mission_id: mission_id.parse().map_err(|_| {
                BridgeError::invalid_argument("mission_id must be the mission Id string")
            })?,
            expected_revision: term_contracts::U64String::new(expected_revision)
                .expect("revision fits the contract range"),
            action,
        })
        .expect("mission control params serialize");
        self.control
            .call("mctl", methods::MISSION_CONTROL, params)
            .await
    }

    /// `mission.decision.answer`(결정 옵션 확정 — CAS).
    pub async fn mission_decision_answer(
        &self,
        request_id: &Id,
        mission_id: &str,
        expected_revision: u64,
        decision_id: &str,
        option_id: Option<String>,
    ) -> Result<serde_json::Value, BridgeError> {
        let params = serde_json::to_value(MissionDecisionAnswerParams {
            request_id: request_id.clone(),
            mission_id: mission_id.parse().map_err(|_| {
                BridgeError::invalid_argument("mission_id must be the mission Id string")
            })?,
            expected_revision: term_contracts::U64String::new(expected_revision)
                .expect("revision fits the contract range"),
            decision_id: decision_id.parse().map_err(|_| {
                BridgeError::invalid_argument("decision_id must be the decision Id string")
            })?,
            option_id,
            answer_ref: None,
        })
        .expect("mission decision params serialize");
        self.control
            .call("mdec", methods::MISSION_DECISION_ANSWER, params)
            .await
    }

    /// `repository.inspect`(미션 생성 플로우의 첫 단계 — 저장소 경로를
    /// canonical 경로·head OID·clean 여부·오염 경로로 검증한다). 결과는
    /// 필드 수가 적어 UI가 전부 쓰므로 계약 타입을 그대로 typed로 돌린다.
    pub async fn repository_inspect(
        &self,
        path: &str,
    ) -> Result<RepositoryInspectResult, BridgeError> {
        let params = serde_json::to_value(RepositoryInspectParams {
            path: path.to_string(),
        })
        .expect("repository inspect params serialize");
        let value = self
            .control
            .call("rinsp", methods::REPOSITORY_INSPECT, params)
            .await?;
        from_result(value, "repository.inspect")
    }

    /// 텍스트를 아티팩트로 업로드(원본 uploadTextArtifact 이식 — 목표·
    /// 메시지 본문이 ArtifactRef로 가는 공통 경로). begin → 데몬이 정한
    /// chunk_bytes 단위 write → commit. mission_id는 None이면 mission
    /// 생성 전 staging, Some이면 해당 mission 대화 본문(계약 주석 참조).
    /// 빈 텍스트도 유효(sha256 of empty).
    pub async fn artifact_upload_text(
        &self,
        request_id: &Id,
        mission_id: Option<&Id>,
        text: &str,
        media_type: &str,
    ) -> Result<ArtifactRef, BridgeError> {
        use base64::Engine as _;
        use sha2::{Digest, Sha256};

        let bytes = text.as_bytes();
        let sha256: String = Sha256::digest(bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        let params = serde_json::to_value(ArtifactBeginParams {
            request_id: request_id.clone(),
            mission_id: mission_id.cloned(),
            media_type: media_type.to_string(),
            bytes: term_contracts::U64String::new(bytes.len() as u64)
                .expect("artifact byte count fits the contract range"),
            sha256,
        })
        .expect("artifact begin params serialize");
        let value = self
            .control
            .call("abeg", methods::ARTIFACT_BEGIN, params)
            .await?;
        let begin: ArtifactBeginResult = from_result(value, "artifact.begin")?;

        let chunk = begin.chunk_bytes.max(1) as usize;
        let mut offset = 0usize;
        while offset < bytes.len() {
            let end = (offset + chunk).min(bytes.len());
            let data_b64 = base64::engine::general_purpose::STANDARD.encode(&bytes[offset..end]);
            let params = serde_json::to_value(ArtifactWriteParams {
                upload_id: begin.upload_id.clone(),
                offset: term_contracts::U64String::new(offset as u64)
                    .expect("artifact offset fits the contract range"),
                data_b64,
            })
            .expect("artifact write params serialize");
            let value = self
                .control
                .call("awrt", methods::ARTIFACT_WRITE, params)
                .await?;
            let _written: ArtifactWriteResult = from_result(value, "artifact.write")?;
            offset = end;
        }

        let params = serde_json::to_value(ArtifactCommitParams {
            upload_id: begin.upload_id,
        })
        .expect("artifact commit params serialize");
        let value = self
            .control
            .call("acmt", methods::ARTIFACT_COMMIT, params)
            .await?;
        from_result(value, "artifact.commit")
    }

    /// `mission.create`(패리티 갭 1위 마지막 호출). params를 그대로 전달하고
    /// 결과는 원시 값으로 돌려 UI가 mission_id·revision만 읽게 한다
    /// (SnapshotPage와 같은 투영 방식).
    pub async fn mission_create(
        &self,
        params: MissionCreateParams,
    ) -> Result<serde_json::Value, BridgeError> {
        let params = serde_json::to_value(params).expect("mission create params serialize");
        self.control
            .call("mnew", methods::MISSION_CREATE, params)
            .await
    }

    /// 미션 대화 메시지 전송(원본 messageSubmission 최소 경로 — 갭 2위):
    /// 본문을 mission 귀속 아티팩트로 올리고 Lead 대화(target None)로
    /// `mission.message`를 보낸다(CAS expected_revision). 재시도 의미론은
    /// 원본과 같다 — 같은 request_id로 서버 영수증이 있으면 멱등.
    pub async fn mission_send_message(
        &self,
        mission_id: &str,
        expected_revision: u64,
        body: &str,
    ) -> Result<serde_json::Value, BridgeError> {
        let mission = mission_id.parse().map_err(|_| {
            BridgeError::invalid_argument("mission_id must be the mission Id string")
        })?;
        let upload_request = Id::generate();
        let body_ref = self
            .artifact_upload_text(
                &upload_request,
                Some(&mission),
                body,
                "text/plain; charset=utf-8",
            )
            .await?;
        let params = serde_json::to_value(MissionMessageParams {
            request_id: Id::generate(),
            mission_id: mission,
            expected_revision: term_contracts::U64String::new(expected_revision)
                .expect("revision fits the contract range"),
            target_task_id: None,
            body_ref,
            supersedes_message_id: None,
        })
        .expect("mission message params serialize");
        self.control
            .call("mmsg", methods::MISSION_MESSAGE, params)
            .await
    }

    /// `runtime.detect`(설치된 런타임 실측 — codex/claude/opencode 순.
    /// 원본 QuickSetup 대응: 폼 프리필 재료).
    pub async fn runtime_detect(
        &self,
    ) -> Result<term_contracts::mission::rpc::RuntimeDetectResult, BridgeError> {
        let value = self
            .control
            .call("rdet", methods::RUNTIME_DETECT, serde_json::json!({}))
            .await?;
        from_result(value, "runtime.detect")
    }

    /// `mission.snapshot`에서 특정 후보(Candidate)를 찾는다(결과 가져오기
    /// 명령용 — 없으면 None, 오류가 아니다).
    pub async fn mission_candidate(
        &self,
        mission_id: &str,
        candidate_id: &str,
    ) -> Result<Option<term_contracts::mission::types::Candidate>, BridgeError> {
        use term_contracts::mission::types::Entity;
        let value = self.mission_snapshot(mission_id).await?;
        let page: term_contracts::mission::types::SnapshotPage = serde_json::from_value(value)
            .map_err(|e| {
                BridgeError::invalid_argument(format!("mission snapshot decode failed: {e}"))
            })?;
        Ok(page
            .entities
            .into_iter()
            .find_map(|entity| match entity {
                Entity::Candidate(candidate) if candidate.id.as_str() == candidate_id => {
                    Some(*candidate)
                }
                _ => None,
            }))
    }

    /// `mission.snapshot`에서 대화 메시지를 뽑아 본문까지 읽는다(LeadConversation
    /// 대응 — created_at 오름차순, 최근 `limit`건만. 본문은 body_ref 아티팩트
    /// 열람, 실패한 본문은 None으로 행만 살린다).
    pub async fn mission_messages(
        &self,
        mission_id: &str,
        limit: usize,
    ) -> Result<Vec<term_contracts::mission::types::Message>, BridgeError> {
        use term_contracts::mission::types::Entity;
        let value = self.mission_snapshot(mission_id).await?;
        let page: term_contracts::mission::types::SnapshotPage = serde_json::from_value(value)
            .map_err(|e| {
                BridgeError::invalid_argument(format!("mission snapshot decode failed: {e}"))
            })?;
        let mut messages: Vec<term_contracts::mission::types::Message> = page
            .entities
            .into_iter()
            .filter_map(|entity| match entity {
                Entity::Message(message) => Some(*message),
                _ => None,
            })
            .collect();
        // 원본 selectMissionMessages와 같은 전순서: created_at 동률은 id로
        // (ISO-8601 UTC의 문자열 비교 = 시각 순서 — 사이클 563).
        messages.sort_by(|a, b| {
            a.created_at
                .cmp(&b.created_at)
                .then_with(|| a.id.as_str().cmp(b.id.as_str()))
        });
        if messages.len() > limit {
            messages.drain(..messages.len() - limit);
        }
        Ok(messages)
    }

    /// `mission.snapshot`에서 작업(Task)·실행(Run)·검증(Verification) 목록을
    /// 함께 뽑는다(상세 화면 표시 — 한 스냅샷 호출로 세 목록). 작업은
    /// ordinal 오름차순, 검증은 시작 순서대로.
    pub async fn mission_tasks(
        &self,
        mission_id: &str,
    ) -> Result<
        (
            Vec<term_contracts::mission::types::Task>,
            Vec<term_contracts::mission::types::Run>,
            Vec<term_contracts::mission::types::Verification>,
        ),
        BridgeError,
    > {
        use term_contracts::mission::types::Entity;
        let value = self.mission_snapshot(mission_id).await?;
        let page: term_contracts::mission::types::SnapshotPage = serde_json::from_value(value)
            .map_err(|e| {
                BridgeError::invalid_argument(format!("mission snapshot decode failed: {e}"))
            })?;
        let mut tasks = Vec::new();
        let mut runs = Vec::new();
        let mut verifications = Vec::new();
        for entity in page.entities {
            match entity {
                Entity::Task(task) => tasks.push(*task),
                Entity::Run(run) => runs.push(*run),
                Entity::Verification(verification) => verifications.push(*verification),
                _ => {}
            }
        }
        tasks.sort_by_key(|task| task.ordinal);
        verifications.sort_by(|a, b| a.started_at.cmp(&b.started_at));
        Ok((tasks, runs, verifications))
    }

    /// `mission.run.attest_exited`(사용자 "프로세스 소멸 확인" — 종료 증거
    /// 없는 unknown/interrupted 실행의 자리를 놓아준다. 문구 키는 GUI와
    /// 같은 고정값 "process_absent_confirmed" — 감사 기록용).
    pub async fn mission_run_attest_exited(
        &self,
        mission_id: &str,
        expected_revision: u64,
        run_id: &str,
    ) -> Result<serde_json::Value, BridgeError> {
        const PROCESS_ABSENT_ATTESTATION: &str = "process_absent_confirmed";
        let mission = mission_id.parse().map_err(|_| {
            BridgeError::invalid_argument("mission_id must be the mission Id string")
        })?;
        let run = run_id.parse().map_err(|_| {
            BridgeError::invalid_argument("run_id must be the run Id string")
        })?;
        let params = serde_json::to_value(
            term_contracts::mission::rpc::MissionRunAttestExitedParams {
                request_id: Id::generate(),
                mission_id: mission,
                expected_revision: term_contracts::U64String::new(expected_revision)
                    .expect("revision fits the contract range"),
                run_id: run,
                attestation: PROCESS_ABSENT_ATTESTATION.to_string(),
            },
        )
        .expect("run attest params serialize");
        self.control
            .call("mrae", methods::MISSION_RUN_ATTEST_EXITED, params)
            .await
    }

    /// `mission.activity`(run 산출 스트림 한 페이지 — after_offset부터
    /// max_bytes까지. body_ref는 불변 아티팩트라 반복 열람이 안전하다).
    pub async fn mission_activity(
        &self,
        mission_id: &str,
        run_id: &str,
        after_offset: u64,
        max_bytes: u32,
    ) -> Result<MissionActivityResult, BridgeError> {
        let params = serde_json::to_value(MissionActivityParams {
            mission_id: mission_id.parse().map_err(|_| {
                BridgeError::invalid_argument("mission_id must be the mission Id string")
            })?,
            run_id: run_id.parse().map_err(|_| {
                BridgeError::invalid_argument("run_id must be the run Id string")
            })?,
            after_offset: term_contracts::U64String::new(after_offset)
                .expect("activity offset fits the contract range"),
            max_bytes,
        })
        .expect("mission activity params serialize");
        let value = self
            .control
            .call("mact", methods::MISSION_ACTIVITY, params)
            .await?;
        from_result(value, "mission.activity")
    }

    /// `mission.policy.update`(미션 정책 저장 — CAS. 정책과 역할 바인딩을
    /// 함께 전달한다: TUI는 발행 시점 조회 값에서 정책만 바꿔 보낸다).
    pub async fn mission_policy_update(
        &self,
        mission_id: &str,
        expected_revision: u64,
        policy: term_contracts::mission::types::Policy,
        role_bindings: Vec<term_contracts::mission::types::RoleBinding>,
    ) -> Result<serde_json::Value, BridgeError> {
        let params = serde_json::to_value(
            term_contracts::mission::rpc::MissionPolicyUpdateParams {
                request_id: Id::generate(),
                mission_id: mission_id.parse().map_err(|_| {
                    BridgeError::invalid_argument("mission_id must be the mission Id string")
                })?,
                expected_revision: term_contracts::U64String::new(expected_revision)
                    .expect("revision fits the contract range"),
                policy,
                role_bindings,
            },
        )
        .expect("mission policy params serialize");
        self.control
            .call("mpol", methods::MISSION_POLICY_UPDATE, params)
            .await
    }

    /// `mission.task.control`(작업 cancel/retry/reassign — CAS. reassign은
    /// binding_id로 새 바인딩을 고르고, retry도 새 시도에 그 바인딩을
    /// 원자적으로 묶을 수 있다).
    pub async fn mission_task_control(
        &self,
        mission_id: &str,
        expected_revision: u64,
        task_id: &str,
        action: TaskControlAction,
        binding_id: Option<&str>,
    ) -> Result<serde_json::Value, BridgeError> {
        let mission = mission_id.parse().map_err(|_| {
            BridgeError::invalid_argument("mission_id must be the mission Id string")
        })?;
        let task = task_id.parse().map_err(|_| {
            BridgeError::invalid_argument("task_id must be the task Id string")
        })?;
        let binding = match binding_id {
            Some(raw) => Some(raw.parse().map_err(|_| {
                BridgeError::invalid_argument("binding_id must be the binding Id string")
            })?),
            None => None,
        };
        let params = serde_json::to_value(MissionTaskControlParams {
            request_id: Id::generate(),
            mission_id: mission,
            expected_revision: term_contracts::U64String::new(expected_revision)
                .expect("revision fits the contract range"),
            task_id: task,
            action,
            binding_id: binding,
        })
        .expect("task control params serialize");
        self.control
            .call("mtsk", methods::MISSION_TASK_CONTROL, params)
            .await
    }

    /// `mission.finding.resolve`(열린 창을 Dismissed로 — 사용자는 기각만
    /// 가능하고 "fixed"는 엔진만 발행한다). 사유 본문을 아티팩트로 올려
    /// reason_ref로 건넨다(mission.message와 같은 upload→mutate 관례).
    pub async fn mission_finding_resolve(
        &self,
        mission_id: &str,
        expected_revision: u64,
        finding_id: &str,
        reason: &str,
    ) -> Result<serde_json::Value, BridgeError> {
        let mission = mission_id.parse().map_err(|_| {
            BridgeError::invalid_argument("mission_id must be the mission Id string")
        })?;
        let finding = finding_id.parse().map_err(|_| {
            BridgeError::invalid_argument("finding_id must be the finding Id string")
        })?;
        let upload_request = Id::generate();
        let reason_ref = self
            .artifact_upload_text(
                &upload_request,
                Some(&mission),
                reason,
                "text/plain; charset=utf-8",
            )
            .await?;
        let params = serde_json::to_value(
            term_contracts::mission::rpc::MissionFindingResolveParams {
                request_id: Id::generate(),
                mission_id: mission,
                expected_revision: term_contracts::U64String::new(expected_revision)
                    .expect("revision fits the contract range"),
                finding_id: finding,
                resolution: term_contracts::mission::rpc::FindingResolutionAction::Dismissed,
                reason_ref,
            },
        )
        .expect("finding resolve params serialize");
        self.control
            .call("mfrs", methods::MISSION_FINDING_RESOLVE, params)
            .await
    }

    /// 미션 결과 수락(갭 2위 — 원본 ResultReview 최소 경로). 검증 승인
    /// 목록·재조정 실행 승인은 서버가 acceptance_ready로 전부 강제 검증한다
    /// — TUI는 인간 요구사항 승인(사용자가 a를 누른 행위 자체가 "직접
    /// 확인했다" 선언)만 전달하고, 통과 검증은 서버 판정에 맡긴다.
    /// 부족하면 구체적 거부 사유가 돌아온다(오류 안내로 표시).
    pub async fn mission_accept(
        &self,
        mission_id: &str,
        expected_revision: u64,
        candidate_id: &str,
        human_requirement_ids: Vec<String>,
    ) -> Result<serde_json::Value, BridgeError> {
        let parse_id = |text: &str| {
            text.parse().map_err(|_| {
                BridgeError::invalid_argument("mission/candidate ids must be Id strings")
            })
        };
        let params = serde_json::to_value(MissionAcceptParams {
            request_id: Id::generate(),
            mission_id: parse_id(mission_id)?,
            expected_revision: term_contracts::U64String::new(expected_revision)
                .expect("revision fits the contract range"),
            candidate_id: parse_id(candidate_id)?,
            // 통과 검증 승인은 서버가 자동 승인 대상을 구분한다(원본
            // acceptancePayload의 observed 필터와 같은 역할을 서버 검증이
            // 담당 — 빈 목록은 "사용자가 추가 승인할 게 없다"는 최소 선언).
            acknowledged_verification_ids: Vec::new(),
            human_requirement_ids: human_requirement_ids
                .iter()
                .map(|id| parse_id(id))
                .collect::<Result<Vec<_>, _>>()?,
            acknowledged_reconciled_run_ids: None,
        })
        .expect("mission accept params serialize");
        self.control
            .call("macc", methods::MISSION_ACCEPT, params)
            .await
    }

    /// 모델 바인딩 목록(갭 3위 — 미션 생성의 사전 조건 조회). params 없는
    /// RPC(계약이 BindingListResult만 정의).
    pub async fn binding_list(&self) -> Result<BindingListResult, BridgeError> {
        let value = self
            .control
            .call("blst", methods::BINDING_LIST, serde_json::Value::Null)
            .await?;
        from_result(value, "binding.list")
    }

    /// 바인딩 probe(설치 상태·실행 가능한 모델 목록 — 데몬이 실측).
    pub async fn binding_probe(&self, binding_id: &str) -> Result<BindingProbeResult, BridgeError> {
        let params = serde_json::to_value(BindingProbeParams {
            binding_id: binding_id.parse().map_err(|_| {
                BridgeError::invalid_argument("binding_id must be the binding Id string")
            })?,
        })
        .expect("binding probe params serialize");
        let value = self
            .control
            .call("bprb", methods::BINDING_PROBE, params)
            .await?;
        from_result(value, "binding.probe")
    }

    /// 미션 템플릿 목록(갭 4위 — repository 필터 없는 전체 조회).
    pub async fn template_list(&self) -> Result<TemplateListResult, BridgeError> {
        let params = serde_json::to_value(TemplateListParams {
            repository_id: None,
        })
        .expect("template list params serialize");
        let value = self
            .control
            .call("tlst", methods::TEMPLATE_LIST, params)
            .await?;
        from_result(value, "template.list")
    }

    /// 저장소 검증 명령 목록(갭 4위 — repository_id 필수, 저장소 검사 결과에서).
    pub async fn verification_list(
        &self,
        repository_id: &str,
    ) -> Result<VerificationListResult, BridgeError> {
        let params = serde_json::to_value(VerificationListParams {
            repository_id: repository_id.parse().map_err(|_| {
                BridgeError::invalid_argument("repository_id must be the repository Id string")
            })?,
        })
        .expect("verification list params serialize");
        let value = self
            .control
            .call("vlst", methods::VERIFICATION_LIST, params)
            .await?;
        from_result(value, "verification.list")
    }

    /// 아티팩트 전문 열람(갭 5위 — 미션 목표 등 산출물 보기). 청크 읽기
    /// 루프(artifact.read의 next_offset·complete)를 모아 UTF-8 텍스트로
    /// 돌린다. 원본 업로드 상한(256KiB)과 같은 규모로 자른다 — 그보다
    /// 큰 아티팩트는 처음 256KiB만.
    pub async fn artifact_read_text(&self, artifact_id: &str) -> Result<String, BridgeError> {
        use base64::Engine as _;

        const MAX_BYTES: u64 = 256 * 1024;
        let artifact: Id = artifact_id.parse().map_err(|_| {
            BridgeError::invalid_argument("artifact_id must be the artifact Id string")
        })?;
        let mut bytes: Vec<u8> = Vec::new();
        let mut offset: u64 = 0;
        loop {
            let params = serde_json::to_value(ArtifactReadParams {
                artifact_id: artifact.clone(),
                offset: term_contracts::U64String::new(offset)
                    .expect("artifact offset fits the contract range"),
                max_bytes: 65_536,
            })
            .expect("artifact read params serialize");
            let value = self
                .control
                .call("ardr", methods::ARTIFACT_READ, params)
                .await?;
            let page: ArtifactReadResult = from_result(value, "artifact.read")?;
            bytes.extend(
                base64::engine::general_purpose::STANDARD
                    .decode(&page.data_b64)
                    .map_err(|error| {
                        BridgeError::invalid_argument(format!("artifact chunk base64: {error}"))
                    })?,
            );
            offset = page.next_offset.get();
            if page.complete || offset >= MAX_BYTES {
                break;
            }
        }
        bytes.truncate(MAX_BYTES as usize);
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }

    /// 최근 예약 목록(갭 6위 — schedule.list, 세션 좁힘 없는 전체).
    pub async fn schedule_list(
        &self,
    ) -> Result<term_contracts::schedule::ScheduleListResult, BridgeError> {
        let params =
            serde_json::to_value(term_contracts::schedule::ScheduleListParams { session_id: None })
                .expect("schedule list params serialize");
        let value = self
            .control
            .call("slst", term_contracts::rpc::methods::SCHEDULE_LIST, params)
            .await?;
        from_result(value, "schedule.list")
    }

    /// 워크스페이스 사용량(갭 6위 — workspace.usage). None=전체 미션,
    /// Some=해당 미션만(미션 상세 연결).
    pub async fn workspace_usage(
        &self,
        mission_id: Option<&str>,
    ) -> Result<WorkspaceUsageResult, BridgeError> {
        let mission_id = match mission_id {
            None => None,
            Some(text) => Some(text.parse().map_err(|_| {
                BridgeError::invalid_argument("mission_id must be the mission Id string")
            })?),
        };
        let params = serde_json::to_value(WorkspaceUsageParams { mission_id })
            .expect("workspace usage params serialize");
        let value = self
            .control
            .call("wuse", methods::WORKSPACE_USAGE, params)
            .await?;
        from_result(value, "workspace.usage")
    }

    /// 예약 취소(갭 6위 조작 — schedule.cancel, pending만). 이미 발사했거나
    /// 종착점이면 cancelled=false(에러가 아니다 — 계약 주석).
    pub async fn schedule_cancel(&self, schedule_id: &str) -> Result<bool, BridgeError> {
        #[derive(serde::Deserialize)]
        struct Cancelled {
            cancelled: bool,
        }
        let params = serde_json::to_value(term_contracts::schedule::ScheduleCancelParams {
            schedule_id: schedule_id.parse().map_err(|_| {
                BridgeError::invalid_argument("schedule_id must be the schedule Id string")
            })?,
        })
        .expect("schedule cancel params serialize");
        let value = self
            .control
            .call(
                "scxl",
                term_contracts::rpc::methods::SCHEDULE_CANCEL,
                params,
            )
            .await?;
        let cancelled: Cancelled = from_result(value, "schedule.cancel")?;
        Ok(cancelled.cancelled)
    }

    /// 미션 워크스페이스 정리(갭 6위 조작 — workspace.cleanup, 미션별).
    pub async fn workspace_cleanup(
        &self,
        mission_id: &str,
    ) -> Result<WorkspaceCleanupResult, BridgeError> {
        let params = serde_json::to_value(WorkspaceCleanupParams {
            request_id: Id::generate(),
            mission_id: mission_id.parse().map_err(|_| {
                BridgeError::invalid_argument("mission_id must be the mission Id string")
            })?,
        })
        .expect("workspace cleanup params serialize");
        let value = self
            .control
            .call("wcln", methods::WORKSPACE_CLEANUP, params)
            .await?;
        from_result(value, "workspace.cleanup")
    }

    /// 예약 생성(갭 6위 마지막 조작 — schedule.create). fire_at_ms는 생성
    /// 시각보다 미래여야 한다(계약 — 호출부가 now+N분으로 계산).
    pub async fn schedule_create(
        &self,
        session_id: &str,
        command: &str,
        fire_at_ms: u64,
    ) -> Result<serde_json::Value, BridgeError> {
        let params = serde_json::to_value(term_contracts::schedule::ScheduleCreateParams {
            session_id: session_id.parse().map_err(|_| {
                BridgeError::invalid_argument("session_id must be the session Id string")
            })?,
            command: command.to_string(),
            fire_at_ms,
        })
        .expect("schedule create params serialize");
        self.control
            .call(
                "snew",
                term_contracts::rpc::methods::SCHEDULE_CREATE,
                params,
            )
            .await
    }

    /// 바인딩 저장(편집 폼 1/3 — binding.save). expected_revision 0은
    /// 신규 생성, 그 외 저장된 revision에 대한 CAS. 데몬 소유 필드
    /// (local_evidence 등)는 클라이언트가 보내도 폐기된다(계약 주석) —
    /// 조회한 Binding을 그대로 되돌려 보내도 안전하다.
    pub async fn binding_save(
        &self,
        expected_revision: u64,
        binding: term_contracts::mission::types::Binding,
    ) -> Result<serde_json::Value, BridgeError> {
        let params = serde_json::to_value(BindingSaveParams {
            request_id: Id::generate(),
            expected_revision: term_contracts::U64String::new(expected_revision)
                .expect("revision fits the contract range"),
            binding,
        })
        .expect("binding save params serialize");
        self.control
            .call("bsav", methods::BINDING_SAVE, params)
            .await
    }

    /// 검증 명령 저장(편집 폼 2/3 — verification.save, CAS).
    pub async fn verification_save(
        &self,
        expected_revision: u64,
        command: term_contracts::mission::types::VerificationCommand,
    ) -> Result<serde_json::Value, BridgeError> {
        let params = serde_json::to_value(VerificationSaveParams {
            request_id: Id::generate(),
            expected_revision: term_contracts::U64String::new(expected_revision)
                .expect("revision fits the contract range"),
            command,
        })
        .expect("verification save params serialize");
        self.control
            .call("vsav", methods::VERIFICATION_SAVE, params)
            .await
    }

    /// 템플릿 저장(편집 폼 3/3 — template.save, CAS).
    pub async fn template_save(
        &self,
        expected_revision: u64,
        template: term_contracts::mission::types::TeamTemplate,
    ) -> Result<serde_json::Value, BridgeError> {
        let params = serde_json::to_value(TemplateSaveParams {
            request_id: Id::generate(),
            expected_revision: term_contracts::U64String::new(expected_revision)
                .expect("revision fits the contract range"),
            template,
        })
        .expect("template save params serialize");
        self.control
            .call("tsav", methods::TEMPLATE_SAVE, params)
            .await
    }
}
