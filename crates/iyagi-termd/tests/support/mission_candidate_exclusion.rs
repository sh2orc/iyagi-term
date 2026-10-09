use super::*;

#[path = "mission_exclusion_cycles.rs"]
mod cycles;

fn replanning(required: bool) -> AdapterFactory {
    let initial = resolving_factory("resolve");
    Arc::new(move |run| {
        let ctx = context(run);
        let kind = ctx["task_kind"].as_str().unwrap();
        if kind == "plan" && ctx["plan_revision"] == 0 {
            let tasks = ["a", "b", "c"].iter().map(|key| serde_json::from_value(json!({
                "local_key":key,"title":key,"kind":"implement","role":"builder","required":*key == "a" || required,
                "parent_key":null,"depends_on_keys":if *key == "c" { vec!["b"] } else { vec![] },
                "objective_text":"Create the assigned file","requirement_ids":[ctx["requirements"][0]["id"]],
                "input_artifact_ids":[],"allowed_paths":[if *key == "c" { "tail.txt" } else { "shared.txt" }],
                "expected_outputs":["patch"],"verification_ids":[],"specialty":null,"binding_id":null,"replacement_of":null
            })).unwrap()).collect();
            return Ok(scripted(script(ProviderResult::Plan {
                based_on_plan_revision: 0,
                tasks,
                retire_task_ids: vec![],
                rationale_text: "Conflict followed by a dependent writer".into(),
            })));
        }
        if kind == "plan" {
            let objective: Value =
                serde_json::from_str(ctx["objective"].as_str().unwrap()).unwrap();
            assert_eq!(objective["kind"], "integration_exclusion");
            assert!(
                ctx["candidate"].is_null(),
                "excluded content cannot be a repair base"
            );
            let excluded = objective["excluded_task_ids"].as_array().unwrap();
            let tasks: Vec<_> = ctx["tasks"].as_array().unwrap().iter()
                .filter(|t| excluded.contains(&t["id"]) && t["required"] == true).enumerate().map(|(n,t)| {
                    let output = format!("replacement{n}.txt");
                    serde_json::from_value(json!({"local_key":format!("replacement{n}"),"title":format!("Replacement {n}"),
                        "kind":"implement","role":"builder","required":true,"parent_key":null,"depends_on_keys":[],
                        "objective_text":"Replace the excluded implementation while preserving requirements",
                        "requirement_ids":t["contract"]["requirement_ids"],"input_artifact_ids":[],"allowed_paths":[output],
                        "expected_outputs":["patch"],"verification_ids":[],"specialty":null,"binding_id":null,"replacement_of":t["id"]})).unwrap()
                }).collect();
            assert_eq!(tasks.len(), if required { 2 } else { 0 });
            return Ok(scripted(script(ProviderResult::Plan {
                based_on_plan_revision: ctx["plan_revision"].as_u64().unwrap() as u32,
                tasks,
                retire_task_ids: vec![],
                rationale_text: "Preserve history and replace the excluded required work".into(),
            })));
        }
        if kind == "review" {
            let path = run.workspace.as_ref().unwrap();
            assert!(path.join("shared.txt").exists());
            assert!(
                !path.join("tail.txt").exists(),
                "dependent source was excluded as well"
            );
            for index in 0..2 {
                assert_eq!(
                    path.join(format!("replacement{index}.txt")).exists(),
                    required
                );
            }
            return Ok(scripted(script(ProviderResult::Review {
                candidate_id: serde_json::from_value(ctx["candidate"]["id"].clone()).unwrap(),
                report_text: "Checked retained and replaced inputs on the new candidate".into(),
                findings: vec![],
            })));
        }
        assert_ne!(
            kind, "integrate",
            "exclusion must not invoke the old resolver"
        );
        initial(run)
    })
}

fn answer(rig: &Rig) -> Value {
    let snapshot = rig.snapshot();
    let decision = snapshot
        .decisions
        .iter()
        .find(|d| d.kind == DecisionKind::Conflict && d.state == DecisionState::Open)
        .unwrap();
    assert!(decision.options.iter().any(|o| o.id == "exclude_candidate"));
    json!({"request_id":Id::generate(),"mission_id":rig.id,"expected_revision":snapshot.mission.revision,
        "decision_id":decision.id,"option_id":"exclude_candidate","answer_ref":null})
}

fn paused_conflict() -> (Rig, tokio::runtime::Runtime, MissionActor) {
    let rig = Rig::new(true, &["ls-files", "--error-unmatch", "shared.txt"]);
    let (runtime, supervisor, _) = actor(&rig, 12 << 30);
    let mut worker = rig
        .actor(replanning(true))
        .with_deterministic_exec(supervisor, runtime.handle().clone());
    rig.tick_until(&mut worker, |s| {
        s.decisions
            .iter()
            .any(|d| d.kind == DecisionKind::Conflict && d.state == DecisionState::Open)
    });
    control(&rig, "pause");
    rig.tick_until(&mut worker, |s| s.mission.state == MissionState::Paused);
    (rig, runtime, worker)
}

#[test]
fn replacement_plan_cannot_waive_excluded_work_or_depend_on_its_successful_history() {
    let (rig, _runtime, mut worker) = paused_conflict();
    rpc(
        &rig.service,
        &rig.conn,
        "mission.decision.answer",
        answer(&rig),
    );
    let before = rig.snapshot();
    let lead = before
        .tasks
        .iter()
        .find(|t| t.kind == TaskKind::Plan && t.state == TaskState::Planned)
        .unwrap();
    let proof = body(&rig, &lead.contract.objective_ref);
    let ids: Vec<Id> = serde_json::from_value(proof["excluded_task_ids"].clone()).unwrap();
    let originals: Vec<_> = before
        .tasks
        .iter()
        .filter(|t| ids.contains(&t.id))
        .collect();
    for mode in [
        "empty",
        "missing_replacement",
        "optional",
        "dependency",
        "retire_success",
    ] {
        let mut tasks: Vec<TaskSpec> = originals
            .iter()
            .map(|old| TaskSpec {
                id: Id::generate(),
                title: "New implementation".into(),
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
        let mut retire = vec![];
        match mode {
            "empty" => tasks.clear(),
            "missing_replacement" => {
                tasks.pop();
            }
            "optional" => tasks[0].required = false,
            "dependency" => tasks[0].depends_on.push(originals[0].id.clone()),
            "retire_success" => retire.push(originals[0].id.clone()),
            _ => unreachable!(),
        }
        let proposal = PlanProposal {
            id: Id::generate(),
            mission_id: rig.id.clone(),
            based_on_plan_revision: before.mission.plan_revision,
            tasks,
            retire_task_ids: retire,
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
        let params = json!({"request_id":Id::generate(),"mission_id":rig.id,"expected_revision":before.mission.revision,"proposal_ref":reference});
        assert!(
            rig.service
                .handle(&rig.conn, "mission.plan.apply", &params)
                .is_err(),
            "{mode}"
        );
        assert_eq!(rig.snapshot().mission, before.mission, "{mode}");
        assert_eq!(rig.snapshot().tasks, before.tasks, "{mode}");
    }
    // A failed replacement may itself be replaced without requiring two
    // implementations of the same original work.
    let mut retired = originals[0].clone();
    retired.id = Id::generate();
    retired.state = TaskState::Superseded;
    retired.replacement_of = Some(originals[0].id.clone());
    retired.ordinal = before.tasks.iter().map(|t| t.ordinal).max().unwrap() + 1;
    retired.active_run_id = None;
    retired.workspace_id = None;
    workflow::commit_upserts(
        &rig.service,
        before.mission.clone(),
        "fixture.exclusion",
        "replacement history",
        MissionEventType::Changed,
        vec![Entity::Task(Box::new(retired.clone()))],
    )
    .unwrap();
    let tasks = originals
        .iter()
        .enumerate()
        .map(|(index, old)| TaskSpec {
            id: Id::generate(),
            title: "Required successor".into(),
            kind: old.kind,
            role: old.role,
            required: true,
            parent_task_id: None,
            depends_on: vec![],
            contract: old.contract.clone(),
            binding_id: old.binding_id.clone(),
            replacement_of: Some(if index == 0 {
                retired.id.clone()
            } else {
                old.id.clone()
            }),
        })
        .collect();
    let current = rig.snapshot();
    let proposal = PlanProposal {
        id: Id::generate(),
        mission_id: rig.id.clone(),
        based_on_plan_revision: current.mission.plan_revision,
        tasks,
        retire_task_ids: vec![],
        rationale_ref: current.mission.goal_ref.clone(),
    };
    let artifacts = ArtifactStore::new(rig.storage.clone(), rig.dir.path().join("missions"));
    let reference = workflow::store_artifact(
        &artifacts,
        &rig.id,
        "application/json",
        &serde_json::to_vec(&proposal).unwrap(),
    )
    .unwrap();
    rpc(
        &rig.service,
        &rig.conn,
        "mission.plan.apply",
        json!({"request_id":Id::generate(),"mission_id":rig.id,
        "expected_revision":current.mission.revision,"proposal_ref":reference}),
    );
    assert_eq!(
        rig.snapshot().mission.plan_revision,
        current.mission.plan_revision + 1
    );
    worker.shutdown();
}

#[test]
fn exclusion_rechecks_budget_current_input_and_native_exit() {
    for mode in ["budget", "candidate_changed", "missing_exec"] {
        let (rig, _runtime, mut worker) = paused_conflict();
        let mut snapshot = rig.snapshot();
        let mut changes = vec![];
        match mode {
            "budget" => snapshot.mission.policy.max_repair_cycles = 0,
            "candidate_changed" => snapshot.mission.candidate_id = Some(Id::generate()),
            "missing_exec" => {
                let mut run = internal_run(&snapshot).unwrap().clone();
                run.exec_id = Some(Id::generate());
                changes.push(Entity::Run(Box::new(run)));
            }
            _ => unreachable!(),
        }
        workflow::commit_upserts(
            &rig.service,
            snapshot.mission,
            "fixture.exclusion",
            "fault",
            MissionEventType::Changed,
            changes,
        )
        .unwrap();
        let before = rig.snapshot();
        assert!(
            rig.service
                .handle(&rig.conn, "mission.decision.answer", &answer(&rig))
                .is_err(),
            "{mode}"
        );
        assert_eq!(rig.snapshot().mission, before.mission, "{mode}");
        assert_eq!(rig.snapshot().decisions, before.decisions, "{mode}");
        assert_eq!(rig.snapshot().tasks, before.tasks, "{mode}");
        worker.shutdown();
    }
}

#[test]
fn exclusion_rebuilds_required_dependents_and_can_omit_optional_inputs() {
    for required in [true, false] {
        let rig = Rig::new(true, &["ls-files", "--error-unmatch", "shared.txt"]);
        let (runtime, supervisor, _) = actor(&rig, 12 << 30);
        let mut actor = rig
            .actor(replanning(required))
            .with_deterministic_exec(supervisor.clone(), runtime.handle().clone());
        rig.tick_until(&mut actor, |s| {
            s.decisions
                .iter()
                .any(|d| d.kind == DecisionKind::Conflict && d.state == DecisionState::Open)
        });
        control(&rig, "pause");
        rig.tick_until(&mut actor, |s| s.mission.state == MissionState::Paused);
        let before = rig.snapshot();
        let conflict = before
            .decisions
            .iter()
            .find(|d| d.kind == DecisionKind::Conflict)
            .unwrap();
        let conflicting: Id = serde_json::from_value(
            body(&rig, &conflict.question_ref)["conflict"]["candidate_id"].clone(),
        )
        .unwrap();
        let params = answer(&rig);
        fault(&rig,"CREATE TRIGGER hold_exclusion BEFORE INSERT ON orch_tasks WHEN json_extract(NEW.document_json, '$.kind') = 'plan' BEGIN SELECT RAISE(ABORT, 'exclusion store outage'); END;");
        assert!(rig
            .service
            .handle(&rig.conn, "mission.decision.answer", &params)
            .is_err());
        assert_eq!(rig.snapshot().mission, before.mission);
        assert_eq!(rig.snapshot().tasks, before.tasks);
        fault(&rig, "DROP TRIGGER hold_exclusion;");
        let receipt = rpc(
            &rig.service,
            &rig.conn,
            "mission.decision.answer",
            params.clone(),
        );
        assert_eq!(
            rpc(&rig.service, &rig.conn, "mission.decision.answer", params),
            receipt
        );
        let chosen = rig.snapshot();
        assert_eq!(chosen.mission.phase, Phase::Planning);
        assert!(chosen.mission.candidate_id.is_none());
        assert_eq!(chosen.runs, before.runs);
        assert_eq!(chosen.candidates, before.candidates);
        assert_eq!(chosen.workspaces, before.workspaces);
        for old in before
            .tasks
            .iter()
            .filter(|t| t.state == TaskState::Succeeded)
        {
            assert_eq!(chosen.tasks.iter().find(|t| t.id == old.id), Some(old));
        }
        let lead = chosen
            .tasks
            .iter()
            .find(|t| t.kind == TaskKind::Plan && t.state == TaskState::Planned)
            .unwrap();
        let evidence = body(&rig, &lead.contract.objective_ref);
        assert_eq!(evidence["conflicting_candidate_id"], conflicting.as_str());
        assert_eq!(evidence["excluded_task_ids"].as_array().unwrap().len(), 2);
        for _ in 0..3 {
            actor.tick().unwrap();
        }
        assert_eq!(rig.snapshot().runs, before.runs);
        actor.shutdown();
        let mut actor = rig
            .actor(replanning(required))
            .with_deterministic_exec(supervisor, runtime.handle().clone());
        control(&rig, "resume");
        rig.tick_until(&mut actor, |s| s.mission.phase == Phase::AwaitingAcceptance);
        let after = rig.snapshot();
        let candidate = after
            .candidates
            .iter()
            .find(|c| Some(&c.id) == after.mission.candidate_id.as_ref())
            .unwrap();
        let excluded_runs: Vec<Id> =
            serde_json::from_value(evidence["excluded_run_ids"].clone()).unwrap();
        assert!(candidate
            .source_run_ids
            .iter()
            .all(|id| !excluded_runs.contains(id)));
        assert_eq!(
            body(&rig, &candidate.manifest_ref)["exclusion_decision_ids"],
            json!([conflict.id])
        );
        assert_eq!(
            after
                .tasks
                .iter()
                .filter(|t| t.replacement_of.is_some() && t.state == TaskState::Succeeded)
                .count(),
            if required { 2 } else { 0 }
        );
        for run in &before.runs {
            assert_eq!(after.runs.iter().find(|r| r.id == run.id), Some(run));
        }
        rpc(
            &rig.service,
            &rig.conn,
            "mission.accept",
            json!({"request_id":Id::generate(),"mission_id":rig.id,
            "expected_revision":after.mission.revision,"candidate_id":candidate.id,"acknowledged_verification_ids":after.verifications.iter().map(|v|&v.id).collect::<Vec<_>>(),"human_requirement_ids":[]}),
        );
        assert_eq!(rig.snapshot().mission.state, MissionState::Completed);
        actor.shutdown();
    }
}
