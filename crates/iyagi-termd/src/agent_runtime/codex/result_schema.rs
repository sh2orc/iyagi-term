//! # `turn/start` outputSchema for typed results (ticket O08, 03-adapters.md §2/§3)
//!
//! 03 §2: the model-facing output schema is NOT the wire `AgentResult` — it
//! is the dedicated [`ProviderResult`] DTO whose bodies are plain `*_text`
//! fields. This module derives the JSON Schema handed to codex
//! `turn/start.params.outputSchema` (TurnStartParams, fixtures/ClientRequest.json:
//! "Optional JSON Schema used to constrain the final assistant message for
//! this turn") mechanically from the serde wire contract of `ProviderResult`
//! (`tag = "kind"`, `rename_all = "lowercase"`, `deny_unknown_fields`):
//! a general nested union, or a flat nullable envelope for Plan/Review;
//! `kind` pinned by an enum, ids constrained
//! to the UUIDv4 pattern so fabricated ids fail validation upstream too.
//! The adapter re-validates the final text by deserializing it as
//! `ProviderResult` — a fabricated or malformed id is `RESULT_INVALID`
//! there regardless of what the model did with the schema.

use serde_json::{json, Value};

/// The six ProviderResult kinds, in wire order (`kind` tag values).
pub const RESULT_KINDS: [&str; 6] = ["plan", "report", "patch", "review", "question", "blocked"];

/// UUIDv4 pattern for daemon-issued entity ids (`term_contracts` `Id`).
const ID_PATTERN: &str = "^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$";

/// ASCII `[a-z][a-z0-9_-]{0,63}` — `ProviderTaskSpec::local_key`.
const LOCAL_KEY_PATTERN: &str = "^[a-z][a-z0-9_-]{0,63}$";

fn schema_string() -> Value {
    json!({ "type": "string" })
}

fn schema_id_array() -> Value {
    json!({ "type": "array", "items": { "type": "string", "pattern": ID_PATTERN } })
}

fn schema_string_array() -> Value {
    json!({ "type": "array", "items": { "type": "string" } })
}

fn schema_optional_id() -> Value {
    json!({
        "anyOf": [
            { "type": "null" },
            { "type": "string", "pattern": ID_PATTERN }
        ]
    })
}

fn schema_task_kind() -> Value {
    json!({
        "type": "string",
        "enum": [
            "plan", "research", "design", "implement", "test_author", "review",
            "consult", "diagnose", "integrate", "document", "verify"
        ]
    })
}

fn schema_role() -> Value {
    json!({
        "anyOf": [
            { "type": "null" },
            {
                "type": "string",
                "enum": [
                    "lead", "researcher", "architect", "builder", "test_author",
                    "reviewer", "specialist", "diagnostician", "integrator", "documenter"
                ]
            }
        ]
    })
}

fn schema_expected_output() -> Value {
    json!({
        "type": "array",
        "items": { "type": "string", "enum": ["report", "patch", "review", "verification"] }
    })
}

fn schema_task_spec() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "local_key": { "type": "string", "pattern": LOCAL_KEY_PATTERN },
            "title": schema_string(),
            "kind": schema_task_kind(),
            "role": schema_role(),
            "required": { "type": "boolean" },
            "parent_key": {
                "anyOf": [
                    { "type": "null" },
                    { "type": "string", "pattern": LOCAL_KEY_PATTERN }
                ]
            },
            "depends_on_keys": {
                "type": "array",
                "items": { "type": "string", "pattern": LOCAL_KEY_PATTERN }
            },
            "objective_text": schema_string(),
            "requirement_ids": schema_id_array(),
            "input_artifact_ids": schema_id_array(),
            "allowed_paths": schema_string_array(),
            "expected_outputs": schema_expected_output(),
            "verification_ids": schema_id_array(),
            "specialty": { "anyOf": [{ "type": "null" }, schema_string()] },
            "binding_id": schema_optional_id(),
            "replacement_of": schema_optional_id()
        },
        "required": [
            "local_key", "title", "kind", "role", "required", "parent_key",
            "depends_on_keys", "objective_text", "requirement_ids",
            "input_artifact_ids", "allowed_paths", "expected_outputs",
            "verification_ids", "specialty", "binding_id", "replacement_of"
        ]
    })
}

fn schema_knowledge_draft() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "kind": { "type": "string", "enum": ["fact", "hypothesis", "decision", "question"] },
            "text": schema_string(),
            "source_artifact_ids": schema_id_array(),
            "related_paths": schema_string_array()
        },
        "required": ["kind", "text", "source_artifact_ids", "related_paths"]
    })
}

fn schema_finding_draft() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "severity": { "type": "string", "enum": ["blocking", "major", "minor", "note"] },
            "path": { "anyOf": [{ "type": "null" }, schema_string()] },
            "line": { "anyOf": [{ "type": "null" }, { "type": "integer", "minimum": 0 }] },
            "evidence_text": schema_string(),
            "requirement_id": schema_optional_id()
        },
        "required": ["severity", "path", "line", "evidence_text", "requirement_id"]
    })
}

fn schema_option() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "id": schema_string(),
            "label": schema_string()
        },
        "required": ["id", "label"]
    })
}

/// One variant object: `kind` pinned by an enum, all fields required (the
/// serde struct has no `#[serde(default)]`), `additionalProperties: false`
/// mirroring `deny_unknown_fields`.
fn variant(kind: &str, fields: &[(&str, Value)]) -> Value {
    let properties: serde_json::Map<String, Value> = std::iter::once((
        "kind".to_string(),
        json!({ "type":"string", "enum":[kind] }),
    ))
    .chain(
        fields
            .iter()
            .map(|(name, schema)| (name.to_string(), schema.clone())),
    )
    .collect();
    let required: Vec<String> = std::iter::once("kind".to_string())
        .chain(fields.iter().map(|(name, _)| name.to_string()))
        .collect();
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": properties,
        "required": required
    })
}

/// The `outputSchema` JSON sent with every codex `turn/start`: a closed
/// `result` property with `anyOf` over the six ProviderResult kinds, text bodies and
/// UUIDv4-constrained ids (03 §2 dedicated model DTO).
pub fn provider_result_output_schema() -> Value {
    let variants = vec![
        variant(
            "plan",
            &[
                (
                    "based_on_plan_revision",
                    json!({ "type": "integer", "minimum": 0 }),
                ),
                (
                    "tasks",
                    json!({ "type": "array", "items": schema_task_spec() }),
                ),
                ("retire_task_ids", schema_id_array()),
                ("rationale_text", schema_string()),
            ],
        ),
        variant(
            "report",
            &[
                ("report_text", schema_string()),
                (
                    "knowledge",
                    json!({ "type": "array", "items": schema_knowledge_draft() }),
                ),
            ],
        ),
        variant(
            "patch",
            &[
                ("report_text", schema_string()),
                ("verification_claims", schema_string_array()),
            ],
        ),
        variant(
            "review",
            &[
                (
                    "candidate_id",
                    json!({ "type": "string", "pattern": ID_PATTERN }),
                ),
                ("report_text", schema_string()),
                (
                    "findings",
                    json!({ "type": "array", "items": schema_finding_draft() }),
                ),
            ],
        ),
        variant(
            "question",
            &[
                ("question_text", schema_string()),
                (
                    "options",
                    json!({ "type": "array", "items": schema_option() }),
                ),
            ],
        ),
        variant(
            "blocked",
            &[("code", schema_string()), ("report_text", schema_string())],
        ),
    ];
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {"result": {"anyOf": variants}},
        "required": ["result"]
    })
}

/// Constrain successful results to the assigned task, preserving question and
/// blocked outcomes. Plan/review use a flat nullable envelope for their nested
/// task/finding objects; the decoder still validates the canonical DTO.
pub fn task_result_output_schema(task: Option<term_contracts::mission::types::TaskKind>) -> Value {
    use term_contracts::mission::types::TaskKind;
    let required_kind = match task {
        Some(TaskKind::Plan) => "plan",
        Some(TaskKind::Review) => "review",
        Some(TaskKind::Verify) => "", // Native verification cannot be replaced by a model report.
        Some(kind) if term_core::mission::capability::writes_workspace(kind) => "patch",
        Some(_) => "report",
        None => return provider_result_output_schema(),
    };
    let mut base = provider_result_output_schema();
    if !matches!(task, Some(TaskKind::Plan | TaskKind::Review)) {
        base["properties"]["result"]["anyOf"]
            .as_array_mut()
            .unwrap()
            .retain(|variant| {
                matches!(variant["properties"]["kind"]["enum"][0].as_str(),
                    Some(kind) if kind == required_kind || kind == "question" || kind == "blocked")
            });
        return base;
    }
    let variants = base["properties"]["result"]["anyOf"].as_array().unwrap();
    let mut fields = serde_json::Map::new();
    for variant in variants {
        for (name, schema) in variant["properties"].as_object().unwrap() {
            if name == "kind" {
                continue;
            }
            let active_kinds: Vec<_> = variants
                .iter()
                .filter(|variant| variant["properties"].get(name).is_some())
                .map(|variant| variant["properties"]["kind"]["enum"][0].as_str().unwrap())
                .collect();
            let description = format!(
                "Required non-null when kind is {}. For every other kind this field MUST be null. An active array with no entries MUST be [], never null.",
                active_kinds.join(" or ")
            );
            fields.insert(
                name.clone(),
                json!({"description":description,"anyOf":[{"type":"null"},schema]}),
            );
        }
    }
    fields.insert(
        "kind".into(),
        json!({"type":"string","enum":[required_kind,"question","blocked"]}),
    );
    let names: Vec<_> = fields.keys().cloned().collect();
    json!({"type":"object","additionalProperties":false,
        "description":"Choose kind first, then follow each field's kind rule exactly. Supply every field. Inactive fields must be null; active fields must have their declared value type (empty arrays are [], not null). Do not include knowledge in a plan or review.",
        "properties":{"format":{"type":"string","enum":["iyagi-result-v2"]},
            "result":{"type":"object","additionalProperties":false,"properties":fields,"required":names}},
        "required":["format","result"]})
}

pub(crate) fn parse_flat_result(
    value: Value,
) -> serde_json::Result<term_contracts::mission::types::ProviderResult> {
    use serde::de::Error;
    let fail = || serde_json::Error::custom("invalid flat result envelope");
    let object = value.as_object().ok_or_else(fail)?;
    let base = provider_result_output_schema();
    let variants = base["properties"]["result"]["anyOf"].as_array().unwrap();
    let expected: std::collections::HashSet<_> = variants
        .iter()
        .flat_map(|variant| variant["properties"].as_object().unwrap().keys())
        .collect();
    if object.len() != expected.len() || object.keys().any(|key| !expected.contains(key)) {
        return Err(fail());
    }
    let kind = object
        .get("kind")
        .and_then(Value::as_str)
        .ok_or_else(fail)?;
    let variant = variants
        .iter()
        .find(|variant| variant["properties"]["kind"]["enum"][0] == kind)
        .ok_or_else(fail)?;
    let active = variant["properties"].as_object().unwrap();
    let mut canonical = serde_json::Map::new();
    for (name, value) in object {
        if active.contains_key(name) {
            canonical.insert(name.clone(), value.clone());
        } else if !value.is_null() {
            return Err(fail());
        }
    }
    serde_json::from_value(Value::Object(canonical))
}

#[cfg(test)]
mod tests {
    use super::*;
    use term_contracts::mission::types::{
        DecisionOption, ExpectedOutput, Id, ProviderFindingDraft, ProviderKnowledgeDraft,
        ProviderResult, ProviderTaskSpec,
    };

    fn variant_names(schema: &Value) -> Vec<String> {
        schema["properties"]["result"]["anyOf"]
            .as_array()
            .expect("nested anyOf array")
            .iter()
            .map(|v| {
                v["properties"]["kind"]["enum"][0]
                    .as_str()
                    .expect("kind enum")
                    .to_string()
            })
            .collect()
    }

    #[test]
    fn schema_covers_exactly_the_six_wire_kinds() {
        let schema = provider_result_output_schema();
        assert_eq!(
            variant_names(&schema),
            RESULT_KINDS.map(str::to_string).to_vec()
        );
    }

    #[test]
    fn every_result_roundtrips_and_validates_its_structured_output_envelope() {
        let schema = provider_result_output_schema();
        let names = variant_names(&schema);
        let validator = jsonschema::validator_for(&schema).expect("valid JSON Schema");
        let id = || Id::generate();
        let samples = vec![
            ProviderResult::Plan {
                based_on_plan_revision: 0,
                tasks: vec![ProviderTaskSpec {
                    local_key: "impl-core".into(),
                    title: "구현".into(),
                    kind: term_contracts::mission::types::TaskKind::Implement,
                    role: Some(term_contracts::mission::types::Role::Builder),
                    required: true,
                    parent_key: None,
                    depends_on_keys: Vec::new(),
                    objective_text: "핵심 로직 구현".into(),
                    requirement_ids: vec![id()],
                    input_artifact_ids: Vec::new(),
                    allowed_paths: vec!["src/".into()],
                    expected_outputs: vec![ExpectedOutput::Patch],
                    verification_ids: Vec::new(),
                    specialty: None,
                    binding_id: None,
                    replacement_of: None,
                }],
                retire_task_ids: Vec::new(),
                rationale_text: "최초 계획".into(),
            },
            ProviderResult::Report {
                report_text: "r".into(),
                knowledge: vec![ProviderKnowledgeDraft {
                    kind: term_contracts::mission::types::KnowledgeKind::Fact,
                    text: "k".into(),
                    source_artifact_ids: vec![id()],
                    related_paths: vec!["a/b".into()],
                }],
            },
            ProviderResult::Question {
                question_text: "q".into(),
                options: vec![DecisionOption {
                    id: "yes".into(),
                    label: "예".into(),
                }],
            },
            ProviderResult::Patch {
                report_text: "p".into(),
                verification_claims: vec!["tests pass".into()],
            },
            ProviderResult::Review {
                candidate_id: id(),
                report_text: "rv".into(),
                findings: vec![ProviderFindingDraft {
                    severity: term_contracts::mission::types::FindingSeverity::Major,
                    path: Some("src/x.rs".into()),
                    line: Some(3),
                    evidence_text: "e".into(),
                    requirement_id: None,
                }],
            },
            ProviderResult::Blocked {
                code: "env".into(),
                report_text: "b".into(),
            },
        ];
        for sample in samples {
            let encoded = serde_json::to_value(&sample).expect("encode");
            let kind = encoded["kind"].as_str().expect("tagged").to_string();
            assert!(names.contains(&kind), "kind {kind} missing from schema");
            assert!(crate::agent_runtime::parse_provider_result(encoded.clone()).is_ok());
            let wrapped = json!({"result":encoded});
            assert!(
                validator.is_valid(&wrapped),
                "schema rejected valid {kind} result"
            );
            assert!(crate::agent_runtime::parse_provider_result(wrapped.clone()).is_ok());
            let mut extra = wrapped.clone();
            extra["unexpected"] = json!(true);
            assert!(!validator.is_valid(&extra));
            assert!(crate::agent_runtime::parse_provider_result(extra).is_err());
            let mut invalid = wrapped;
            invalid["result"]["kind"] = json!("invented-kind");
            assert!(!validator.is_valid(&invalid));
            assert!(crate::agent_runtime::parse_provider_result(invalid).is_err());
        }
    }

    #[test]
    fn id_fields_carry_the_uuid_v4_pattern() {
        let schema = provider_result_output_schema();
        let encoded = serde_json::to_string(&schema).expect("encode");
        assert!(encoded.contains("candidate_id"));
        assert!(
            encoded.contains("[89ab]"),
            "UUIDv4 variant nibble pattern present"
        );
    }

    fn flat(canonical: Value) -> Value {
        let schema =
            task_result_output_schema(Some(term_contracts::mission::types::TaskKind::Plan));
        let mut result: serde_json::Map<String, Value> = schema["properties"]["result"]
            ["properties"]
            .as_object()
            .unwrap()
            .keys()
            .map(|name| (name.clone(), Value::Null))
            .collect();
        result.extend(canonical.as_object().unwrap().clone());
        json!({"format":"iyagi-result-v2","result":result})
    }

    #[test]
    fn flat_plan_review_and_non_success_outcomes_keep_their_canonical_contract() {
        use term_contracts::mission::types::TaskKind;
        for (task, success) in [
            (
                TaskKind::Plan,
                json!({"kind":"plan","based_on_plan_revision":0,"tasks":[],"retire_task_ids":[],"rationale_text":"initial"}),
            ),
            (
                TaskKind::Review,
                json!({"kind":"review","candidate_id":Id::generate(),"report_text":"checked","findings":[]}),
            ),
        ] {
            let validator =
                jsonschema::validator_for(&task_result_output_schema(Some(task))).unwrap();
            for canonical in [
                success,
                json!({"kind":"question","question_text":"choose","options":[{"id":"yes","label":"Yes"}]}),
                json!({"kind":"blocked","code":"missing-input","report_text":"need input"}),
            ] {
                let envelope = flat(canonical.clone());
                assert!(validator.is_valid(&envelope));
                let parsed = crate::agent_runtime::parse_provider_result(envelope).unwrap();
                assert_eq!(serde_json::to_value(parsed).unwrap(), canonical);
            }
            assert!(!validator.is_valid(&flat(
                json!({"kind":"report","report_text":"wrong task outcome","knowledge":[]})
            )));
        }
    }

    #[test]
    fn assigned_task_schema_rejects_other_roles_results_and_preserves_questions() {
        use term_contracts::mission::types::TaskKind;
        let patch = json!({"result":{"kind":"patch","report_text":"created assigned file","verification_claims":[]}});
        let report =
            json!({"result":{"kind":"report","report_text":"research findings","knowledge":[]}});
        let plan = json!({"result":{"kind":"plan","based_on_plan_revision":0,"tasks":[],"retire_task_ids":[],"rationale_text":"wrong role"}});
        let question =
            json!({"result":{"kind":"question","question_text":"need input","options":[]}});
        let blocked = json!({"result":{"kind":"blocked","code":"missing-input","report_text":"cannot proceed"}});
        for task in [
            TaskKind::Research,
            TaskKind::Design,
            TaskKind::Implement,
            TaskKind::TestAuthor,
            TaskKind::Consult,
            TaskKind::Diagnose,
            TaskKind::Integrate,
            TaskKind::Document,
            TaskKind::Verify,
        ] {
            let schema = task_result_output_schema(Some(task));
            let validator = jsonschema::validator_for(&schema).unwrap();
            let writer = term_core::mission::capability::writes_workspace(task);
            assert_eq!(validator.is_valid(&patch), writer, "{task:?}");
            assert_eq!(
                validator.is_valid(&report),
                !writer && task != TaskKind::Verify,
                "{task:?}"
            );
            assert!(!validator.is_valid(&plan), "{task:?}");
            for result in [&question, &blocked] {
                assert!(validator.is_valid(result), "{task:?}");
                assert!(crate::agent_runtime::parse_provider_result(result.clone()).is_ok());
            }
        }
        assert_eq!(
            task_result_output_schema(None),
            provider_result_output_schema()
        );
    }

    #[test]
    fn flat_decoder_rejects_inactive_payloads_missing_arrays_and_unknown_fields() {
        let valid = flat(
            json!({"kind":"plan","based_on_plan_revision":0,"tasks":[],"retire_task_ids":[],"rationale_text":"initial"}),
        );
        let mut cases = vec![];
        let mut inactive = valid.clone();
        inactive["result"]["knowledge"] = json!([]);
        cases.push(inactive);
        let mut missing_array = valid.clone();
        missing_array["result"]["retire_task_ids"] = Value::Null;
        cases.push(missing_array);
        let mut missing_field = valid.clone();
        missing_field["result"]
            .as_object_mut()
            .unwrap()
            .remove("code");
        cases.push(missing_field);
        let mut extra = valid.clone();
        extra["result"]["unexpected"] = Value::Null;
        cases.push(extra);
        let mut unknown = valid.clone();
        unknown["format"] = json!("future-format");
        cases.push(unknown);
        let mut invalid_id = flat(
            json!({"kind":"review","candidate_id":"not-an-id","report_text":"checked","findings":[]}),
        );
        cases.push(invalid_id.clone());
        invalid_id["result"]["candidate_id"] = json!(Id::generate());
        invalid_id["result"]["findings"] = json!([{"severity":"note","path":null,"line":null,"evidence_text":"x","requirement_id":null,"extra":true}]);
        cases.push(invalid_id);
        for invalid in cases {
            assert!(
                crate::agent_runtime::parse_provider_result(invalid.clone()).is_err(),
                "accepted {invalid}"
            );
        }
    }

    #[test]
    fn plan_variant_requires_task_text_fields() {
        let schema = provider_result_output_schema();
        let plan = &schema["properties"]["result"]["anyOf"][0];
        assert!(plan["properties"].get("rationale_text").is_some());
        assert!(
            plan["properties"]["tasks"]["items"]["properties"]
                .get("objective_text")
                .is_some(),
            "task objective_text is the model-facing body field"
        );
    }
}
