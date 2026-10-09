//! Required automatic-run capabilities. Optional features never grant access.
use term_contracts::mission::types::{Role, RuntimeCapabilities, TaskKind};

pub fn writes_workspace(kind: TaskKind) -> bool {
    matches!(
        kind,
        TaskKind::Implement | TaskKind::TestAuthor | TaskKind::Integrate | TaskKind::Document
    )
}

pub fn role_kind(role: Role) -> TaskKind {
    match role {
        Role::Lead => TaskKind::Plan,
        Role::Researcher => TaskKind::Research,
        Role::Architect => TaskKind::Design,
        Role::Builder => TaskKind::Implement,
        Role::TestAuthor => TaskKind::TestAuthor,
        Role::Reviewer => TaskKind::Review,
        Role::Specialist => TaskKind::Consult,
        Role::Diagnostician => TaskKind::Diagnose,
        Role::Integrator => TaskKind::Integrate,
        Role::Documenter => TaskKind::Document,
    }
}

pub fn missing(caps: &RuntimeCapabilities, kind: TaskKind) -> Option<&'static str> {
    if kind == TaskKind::Verify {
        return None;
    }
    [
        ("structured_result", &caps.structured_result),
        ("events", &caps.events),
        ("cancel", &caps.cancel),
        if writes_workspace(kind) {
            ("scoped_write", &caps.scoped_write)
        } else {
            ("read_only", &caps.read_only)
        },
    ]
    .into_iter()
    .find_map(|(name, support)| (!support.supported).then_some(name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use term_contracts::mission::types::Support;
    fn caps() -> RuntimeCapabilities {
        let yes = Support {
            supported: true,
            reason_code: None,
        };
        RuntimeCapabilities {
            structured_result: yes.clone(),
            events: yes.clone(),
            cancel: yes.clone(),
            resume: yes.clone(),
            steer: yes.clone(),
            approval_reply: yes.clone(),
            read_only: yes.clone(),
            scoped_write: yes.clone(),
            model_listing: yes.clone(),
            usage: yes.clone(),
            native_terminal_attach: yes,
        }
    }
    #[test]
    fn every_role_requires_its_actual_workspace_access() {
        for (role, write) in [
            (Role::Lead, false),
            (Role::Researcher, false),
            (Role::Architect, false),
            (Role::Builder, true),
            (Role::TestAuthor, true),
            (Role::Reviewer, false),
            (Role::Specialist, false),
            (Role::Diagnostician, false),
            (Role::Integrator, true),
            (Role::Documenter, true),
        ] {
            let kind = role_kind(role);
            assert_eq!(writes_workspace(kind), write);
            let mut current = caps();
            if write {
                current.read_only.supported = false;
            } else {
                current.scoped_write.supported = false;
            }
            assert_eq!(missing(&current, kind), None);
            current.read_only.supported = false;
            current.scoped_write.supported = false;
            assert_eq!(
                missing(&current, kind),
                Some(if write { "scoped_write" } else { "read_only" })
            );
        }
    }
    #[test]
    fn common_requirements_are_mandatory_but_optional_features_are_not() {
        for name in ["structured_result", "events", "cancel"] {
            let mut value = serde_json::to_value(caps()).unwrap();
            value[name]["supported"] = serde_json::json!(false);
            let caps = serde_json::from_value(value).unwrap();
            assert_eq!(missing(&caps, TaskKind::Plan), Some(name));
            assert_eq!(missing(&caps, TaskKind::Verify), None);
        }
        let mut caps = caps();
        for support in [
            &mut caps.resume,
            &mut caps.steer,
            &mut caps.approval_reply,
            &mut caps.model_listing,
            &mut caps.usage,
            &mut caps.native_terminal_attach,
        ] {
            support.supported = false;
        }
        assert_eq!(missing(&caps, TaskKind::Implement), None);
    }
}
