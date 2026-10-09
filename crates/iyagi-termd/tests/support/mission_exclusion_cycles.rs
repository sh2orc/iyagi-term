use super::*;

fn cycle_factory(review_repair: bool) -> AdapterFactory {
    let initial = resolving_factory("resolve");
    Arc::new(move |run| {
        let ctx = context(run);
        let kind = ctx["task_kind"].as_str().unwrap();
        if kind == "plan" && ctx["plan_revision"] != 0 {
            let objective: Value =
                serde_json::from_str(ctx["objective"].as_str().unwrap()).unwrap();
            let tasks = if objective["kind"] == "integration_exclusion" {
                assert!(ctx["candidate"].is_null());
                let excluded = objective["excluded_task_ids"].as_array().unwrap();
                ctx["tasks"].as_array().unwrap().iter()
                    .filter(|t| excluded.contains(&t["id"]) && t["required"] == true && t["kind"] != "plan")
                    .enumerate().map(|(index, old)| {
                        let paths = if old["kind"] == "implement" {
                            // The first replacement conflicts again. Its successor
                            // must satisfy both original and replacement contracts.
                            if !review_repair && ctx["plan_revision"] == 1 {
                                json!(["shared.txt"])
                            } else { json!([format!("fresh-{}.txt", old["id"].as_str().unwrap())]) }
                        } else { json!([]) };
                        serde_json::from_value(json!({
                            "local_key":format!("replacement{index}"),"title":format!("Replacement {index}"),
                            "kind":old["kind"],"role":old["role"],"required":true,"parent_key":null,"depends_on_keys":[],
                            "objective_text":"Replace the excluded work and preserve its original requirements and checks",
                            "requirement_ids":old["contract"]["requirement_ids"],"input_artifact_ids":[],"allowed_paths":paths,
                            "expected_outputs":old["contract"]["expected_outputs"],"verification_ids":old["contract"]["verification_ids"],
                            "specialty":null,"binding_id":null,"replacement_of":old["id"]
                        })).unwrap()
                    }).collect()
            } else {
                assert!(review_repair);
                vec![serde_json::from_value(json!({"local_key":"repair","title":"Repair on prior candidate","kind":"implement",
                    "role":"builder","required":true,"parent_key":null,"depends_on_keys":[],"objective_text":"Repair the review finding",
                    "requirement_ids":[ctx["requirements"][0]["id"]],"input_artifact_ids":[],"allowed_paths":["repair.txt"],
                    "expected_outputs":["patch"],"verification_ids":[],"specialty":null,"binding_id":null,"replacement_of":null})).unwrap()]
            };
            return Ok(scripted(script(ProviderResult::Plan {
                based_on_plan_revision: ctx["plan_revision"].as_u64().unwrap() as u32,
                tasks,
                retire_task_ids: vec![],
                rationale_text: "Replace excluded work while retaining completed history".into(),
            })));
        }
        if kind == "implement" && ctx["task_contract"]["allowed_paths"][0] == "repair.txt" {
            assert_eq!(
                std::fs::read_to_string(run.workspace.as_ref().unwrap().join("shared.txt"))
                    .unwrap(),
                "resolved together\n"
            );
        }
        if kind == "review" {
            let findings = if review_repair && ctx["candidate"]["revision"] == 1 {
                vec![ProviderFindingDraft {
                    severity: FindingSeverity::Major,
                    path: Some("shared.txt".into()),
                    line: Some(1),
                    evidence_text: "Add a repair on the reviewed candidate".into(),
                    requirement_id: None,
                }]
            } else {
                vec![]
            };
            return Ok(scripted(script(ProviderResult::Review {
                candidate_id: serde_json::from_value(ctx["candidate"]["id"].clone()).unwrap(),
                report_text: "Reviewed this exact candidate".into(),
                findings,
            })));
        }
        initial(run)
    })
}

fn conflict(rig: &Rig, worker: &mut MissionActor) {
    rig.tick_until(worker, |s| {
        s.decisions
            .iter()
            .any(|d| d.kind == DecisionKind::Conflict && d.state == DecisionState::Open)
    });
}

fn choose(rig: &Rig, option: &str) {
    let mut params = answer(rig);
    params["option_id"] = json!(option);
    rpc(&rig.service, &rig.conn, "mission.decision.answer", params);
}

fn accept(rig: &Rig) {
    let snapshot = rig.snapshot();
    let verifications: Vec<_> = snapshot
        .verifications
        .iter()
        .filter(|v| Some(&v.candidate_id) == snapshot.mission.candidate_id.as_ref())
        .map(|v| v.id.clone())
        .collect();
    rpc(
        &rig.service,
        &rig.conn,
        "mission.accept",
        json!({"request_id":Id::generate(),"mission_id":rig.id,
        "expected_revision":snapshot.mission.revision,"candidate_id":snapshot.mission.candidate_id,
        "acknowledged_verification_ids":verifications,"human_requirement_ids":[]}),
    );
    assert_eq!(rig.snapshot().mission.state, MissionState::Completed);
}

fn reject_weakened_checks(rig: &Rig, excluded: &[Id]) {
    let before = rig.snapshot();
    let originals: Vec<_> = before
        .tasks
        .iter()
        .filter(|t| excluded.contains(&t.id) && t.required && t.kind != TaskKind::Plan)
        .collect();
    for mode in [
        "drop_verification",
        "drop_review",
        "change_verification_kind",
        "change_review_kind",
        "drop_command",
    ] {
        let mut tasks: Vec<TaskSpec> = originals
            .iter()
            .map(|old| TaskSpec {
                id: Id::generate(),
                title: "Replacement".into(),
                kind: old.kind,
                role: old.role,
                required: true,
                parent_task_id: None,
                depends_on: vec![],
                contract: old.contract.clone(),
                binding_id: old.binding_id.clone(),
                replacement_of: Some(old.id.clone()),
            })
            .collect();
        let kind = if mode.contains("review") {
            TaskKind::Review
        } else {
            TaskKind::Verify
        };
        let index = tasks.iter().position(|t| t.kind == kind).unwrap();
        if mode == "drop_command" {
            tasks[index].contract.verification_ids.clear();
        } else if mode.starts_with("drop_") {
            tasks.remove(index);
        } else {
            tasks[index].kind = TaskKind::Implement;
            tasks[index].role = Some(Role::Builder);
            tasks[index].binding_id = Some(
                before
                    .mission
                    .role_bindings
                    .iter()
                    .find(|b| b.role == Role::Builder)
                    .unwrap()
                    .primary_binding_id
                    .clone(),
            );
            tasks[index].contract.expected_outputs = vec![ExpectedOutput::Patch];
            tasks[index].contract.allowed_paths = vec!["extra.txt".into()];
        }
        let proposal = PlanProposal {
            id: Id::generate(),
            mission_id: rig.id.clone(),
            based_on_plan_revision: before.mission.plan_revision,
            tasks,
            retire_task_ids: vec![],
            rationale_ref: before.mission.goal_ref.clone(),
        };
        let artifacts = ArtifactStore::new(rig.storage.clone(), rig.dir.path().join("missions"));
        let reference = workflow::store_artifact(
            &artifacts,
            &rig.id,
            "application/json",
            &serde_json::to_vec(&proposal).unwrap(),
        )
        .unwrap();
        let error = rig
            .service
            .handle(
                &rig.conn,
                "mission.plan.apply",
                &json!({"request_id":Id::generate(),"mission_id":rig.id,
            "expected_revision":before.mission.revision,"proposal_ref":reference}),
            )
            .err()
            .expect("weakened or stale checks must be rejected");
        assert!(
            error.message.contains("every excluded required task"),
            "{mode}: {error:?}"
        );
        assert_eq!(rig.snapshot().mission, before.mission);
        assert_eq!(rig.snapshot().tasks, before.tasks);
    }
}

fn reject_stale_check_evidence(rig: &Rig, prior: &workflow::MissionEntities) {
    let complete = rig.snapshot();
    let verify = complete
        .verifications
        .iter()
        .find(|v| Some(&v.candidate_id) == complete.mission.candidate_id.as_ref())
        .unwrap();
    let old_review = prior
        .runs
        .iter()
        .find(|r| {
            prior
                .tasks
                .iter()
                .any(|t| t.id == r.task_id && t.kind == TaskKind::Review)
        })
        .unwrap();
    let review = complete
        .runs
        .iter()
        .find(|r| {
            complete.tasks.iter().any(|t| {
                t.id == r.task_id && t.kind == TaskKind::Review && t.replacement_of.is_some()
            })
        })
        .unwrap();
    let mut stale_verify = verify.clone();
    stale_verify.candidate_id = prior.mission.candidate_id.clone().unwrap();
    let mut stale_review = review.clone();
    stale_review.result_ref = old_review.result_ref.clone();
    for (changed, restored) in [
        (
            Entity::Verification(Box::new(stale_verify)),
            Entity::Verification(Box::new(verify.clone())),
        ),
        (
            Entity::Run(Box::new(stale_review)),
            Entity::Run(Box::new(review.clone())),
        ),
    ] {
        workflow::commit_upserts(
            &rig.service,
            rig.snapshot().mission,
            "fixture.exclusion",
            "stale check evidence",
            MissionEventType::Changed,
            vec![changed],
        )
        .unwrap();
        let before = rig.snapshot();
        let ids: Vec<_> = complete
            .verifications
            .iter()
            .map(|v| v.id.clone())
            .collect();
        let error = rig
            .service
            .handle(
                &rig.conn,
                "mission.accept",
                &json!({"request_id":Id::generate(),"mission_id":rig.id,
            "expected_revision":before.mission.revision,"candidate_id":before.mission.candidate_id,
            "acknowledged_verification_ids":ids,"human_requirement_ids":[]}),
            )
            .err()
            .expect("weakened or stale checks must be rejected");
        assert!(
            error.message.contains("every excluded required task"),
            "{error:?}"
        );
        assert_eq!(rig.snapshot().mission, before.mission);
        workflow::commit_upserts(
            &rig.service,
            before.mission,
            "fixture.exclusion",
            "restore check evidence",
            MissionEventType::Changed,
            vec![restored],
        )
        .unwrap();
    }
}

#[test]
fn successive_exclusions_replace_the_whole_history_chain() {
    let rig = Rig::new(
        true,
        &["ls-files", "--error-unmatch", "shared.txt", "tail.txt"],
    );
    let (runtime, supervisor, _) = actor(&rig, 12 << 30);
    let mut worker = rig
        .actor(cycle_factory(false))
        .with_deterministic_exec(supervisor, runtime.handle().clone());
    conflict(&rig, &mut worker);
    choose(&rig, "exclude_candidate");
    conflict(&rig, &mut worker);
    let before = rig.snapshot();
    choose(&rig, "exclude_candidate");
    rig.tick_until(&mut worker, |s| {
        s.mission.phase == Phase::AwaitingAcceptance
    });
    let after = rig.snapshot();
    assert_eq!(after.mission.plan_revision, 3);
    let candidate = after
        .candidates
        .iter()
        .find(|c| Some(&c.id) == after.mission.candidate_id.as_ref())
        .unwrap();
    assert_eq!(
        body(&rig, &candidate.manifest_ref)["exclusion_decision_ids"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    for old in &before.runs {
        assert_eq!(after.runs.iter().find(|r| r.id == old.id), Some(old));
    }
    accept(&rig);
    worker.shutdown();
}

#[test]
fn excluding_a_prior_input_rebuilds_repair_work_and_checks_after_integration() {
    let rig = Rig::new(
        true,
        &["ls-files", "--error-unmatch", "shared.txt", "tail.txt"],
    );
    let (runtime, supervisor, _) = actor(&rig, 12 << 30);
    let mut worker = rig
        .actor(cycle_factory(true))
        .with_deterministic_exec(supervisor.clone(), runtime.handle().clone());
    conflict(&rig, &mut worker);
    choose(&rig, "resolve_and_reintegrate");
    // The reviewed candidate needs a repair. Replaying the original sources
    // conflicts again; now exclusion must invalidate the repair's base too.
    conflict(&rig, &mut worker);
    control(&rig, "pause");
    rig.tick_until(&mut worker, |s| s.mission.state == MissionState::Paused);
    let before = rig.snapshot();
    let prior_candidate = before.mission.candidate_id.clone().unwrap();
    assert_eq!(before.verifications.len(), 1);
    assert_eq!(before.findings.len(), 1);
    choose(&rig, "exclude_candidate");
    let chosen = rig.snapshot();
    let plan = chosen
        .tasks
        .iter()
        .find(|t| t.kind == TaskKind::Plan && t.state == TaskState::Planned)
        .unwrap();
    let proof = body(&rig, &plan.contract.objective_ref);
    let excluded: Vec<Id> = serde_json::from_value(proof["excluded_task_ids"].clone()).unwrap();
    for kind in [TaskKind::Verify, TaskKind::Review] {
        assert!(before
            .tasks
            .iter()
            .any(|t| t.kind == kind && excluded.contains(&t.id)));
    }
    assert!(before
        .tasks
        .iter()
        .any(|t| t.title == "Repair on prior candidate" && excluded.contains(&t.id)));
    reject_weakened_checks(&rig, &excluded);
    worker.shutdown();
    let mut worker = rig
        .actor(cycle_factory(true))
        .with_deterministic_exec(supervisor, runtime.handle().clone());
    control(&rig, "resume");
    rig.tick_until(&mut worker, |s| {
        s.mission.phase == Phase::AwaitingAcceptance
    });
    let after = rig.snapshot();
    let candidate = after
        .candidates
        .iter()
        .find(|c| Some(&c.id) == after.mission.candidate_id.as_ref())
        .unwrap();
    assert_eq!(candidate.revision, 2);
    assert_eq!(candidate.supersedes_id, Some(prior_candidate));
    assert_eq!(after.verifications.len(), 2);
    assert_eq!(
        after
            .verifications
            .iter()
            .filter(|v| v.candidate_id == candidate.id && v.status == VerificationStatus::Passed)
            .count(),
        1
    );
    for old in before
        .tasks
        .iter()
        .filter(|t| t.state == TaskState::Succeeded)
    {
        assert_eq!(after.tasks.iter().find(|t| t.id == old.id), Some(old));
    }
    for old in &before.runs {
        assert_eq!(after.runs.iter().find(|r| r.id == old.id), Some(old));
    }
    assert_eq!(after.findings, before.findings);
    reject_stale_check_evidence(&rig, &before);
    accept(&rig);
    worker.shutdown();
}
