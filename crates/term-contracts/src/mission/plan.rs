//! Pure plan-graph validation (02 §5 dependency rules): duplicate ids, self
//! edges, missing dependency targets, duplicate edges, cross-mission edges,
//! and cycles via Kahn topological sort. The daemon applies this to the union
//! of existing accepted tasks and a new proposal before any insert.

use serde::{Deserialize, Serialize};

/// Minimal node view so both `ProviderTaskSpec` (local keys) and stored
/// `TaskSpec` (UUIDs) validate through the same code.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanNode {
    pub id: String,
    /// Owning mission discriminator; edges across missions are rejected.
    pub mission: String,
    /// Dependencies as raw id/key strings.
    pub deps: Vec<String>,
}

/// Internal plan-graph error vocabulary (not a wire type).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PlanGraphError {
    #[error("duplicate task id {0}")]
    DuplicateId(String),
    #[error("task {0} depends on itself")]
    SelfEdge(String),
    #[error("task {0} depends on missing {1}")]
    MissingDependency(String, String),
    #[error("task {0} repeats dependency {1}")]
    DuplicateEdge(String, String),
    #[error("task {0} in mission {1} depends across missions")]
    CrossMission(String, String),
    #[error("dependency graph has a cycle")]
    Cycle,
}

/// Validate a task set: structural edges first (cheap, precise messages),
/// then Kahn's algorithm for cycles. `Ok(())` also implies a topological
/// order exists.
pub fn validate_plan_graph(nodes: &[PlanNode]) -> Result<(), PlanGraphError> {
    let mut seen = std::collections::HashSet::new();
    for node in nodes {
        if !seen.insert(node.id.as_str()) {
            return Err(PlanGraphError::DuplicateId(node.id.clone()));
        }
    }
    for node in nodes {
        let mut dep_seen = std::collections::HashSet::new();
        for dep in &node.deps {
            if dep == &node.id {
                return Err(PlanGraphError::SelfEdge(node.id.clone()));
            }
            if !dep_seen.insert(dep.as_str()) {
                return Err(PlanGraphError::DuplicateEdge(node.id.clone(), dep.clone()));
            }
            let target = nodes.iter().find(|n| &n.id == dep);
            match target {
                None => {
                    return Err(PlanGraphError::MissingDependency(
                        node.id.clone(),
                        dep.clone(),
                    ))
                }
                Some(target) => {
                    if target.mission != node.mission {
                        return Err(PlanGraphError::CrossMission(
                            node.id.clone(),
                            node.mission.clone(),
                        ));
                    }
                }
            }
        }
    }
    kahn_acyclic(nodes)
        .then_some(())
        .ok_or(PlanGraphError::Cycle)
}

fn kahn_acyclic(nodes: &[PlanNode]) -> bool {
    let index: std::collections::HashMap<&str, usize> = nodes
        .iter()
        .enumerate()
        .map(|(i, n)| (n.id.as_str(), i))
        .collect();
    let mut pending: Vec<usize> = nodes.iter().map(|n| n.deps.len()).collect();
    let mut dependents: Vec<Vec<usize>> = vec![Vec::new(); nodes.len()];
    for (i, node) in nodes.iter().enumerate() {
        for dep in &node.deps {
            if let Some(&j) = index.get(dep.as_str()) {
                dependents[j].push(i);
            }
        }
    }
    let mut ready: Vec<usize> = (0..nodes.len()).filter(|&i| pending[i] == 0).collect();
    let mut visited = 0usize;
    while let Some(i) = ready.pop() {
        visited += 1;
        for &d in &dependents[i] {
            pending[d] -= 1;
            if pending[d] == 0 {
                ready.push(d);
            }
        }
    }
    visited == nodes.len()
}

/// Stable topological order (ties by input order) — used for integration
/// ordering (04 §4: plan topological order, ties by task ordinal).
pub fn topological_order(nodes: &[PlanNode]) -> Result<Vec<String>, PlanGraphError> {
    validate_plan_graph(nodes)?;
    let index: std::collections::HashMap<&str, usize> = nodes
        .iter()
        .enumerate()
        .map(|(i, n)| (n.id.as_str(), i))
        .collect();
    let mut pending: Vec<usize> = nodes.iter().map(|n| n.deps.len()).collect();
    let mut dependents: Vec<Vec<usize>> = vec![Vec::new(); nodes.len()];
    for (i, node) in nodes.iter().enumerate() {
        for dep in &node.deps {
            if let Some(&j) = index.get(dep.as_str()) {
                dependents[j].push(i);
            }
        }
    }
    // FIFO keeps input order among ready nodes.
    let mut ready: std::collections::VecDeque<usize> =
        (0..nodes.len()).filter(|&i| pending[i] == 0).collect();
    let mut order = Vec::with_capacity(nodes.len());
    while let Some(i) = ready.pop_front() {
        order.push(nodes[i].id.clone());
        for &d in &dependents[i] {
            pending[d] -= 1;
            if pending[d] == 0 {
                ready.push_back(d);
            }
        }
    }
    Ok(order)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: &str, mission: &str, deps: &[&str]) -> PlanNode {
        PlanNode {
            id: id.into(),
            mission: mission.into(),
            deps: deps.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn diamond_is_valid_and_ordered() {
        let nodes = [
            node("a", "m", &[]),
            node("b", "m", &["a"]),
            node("c", "m", &["a"]),
        ];
        assert_eq!(validate_plan_graph(&nodes), Ok(()));
        assert_eq!(topological_order(&nodes).unwrap(), vec!["a", "b", "c"]);
    }

    #[test]
    fn structural_failures_are_precise() {
        assert_eq!(
            validate_plan_graph(&[node("a", "m", &["b"]), node("b", "m", &["a"])]),
            Err(PlanGraphError::Cycle)
        );
        assert_eq!(
            validate_plan_graph(&[node("a", "m", &["a"])]),
            Err(PlanGraphError::SelfEdge("a".into()))
        );
        assert_eq!(
            validate_plan_graph(&[node("a", "m", &["missing"])]),
            Err(PlanGraphError::MissingDependency(
                "a".into(),
                "missing".into()
            ))
        );
        assert_eq!(
            validate_plan_graph(&[node("a", "m", &[]), node("a", "m", &[])]),
            Err(PlanGraphError::DuplicateId("a".into()))
        );
        assert_eq!(
            validate_plan_graph(&[node("a", "m", &[]), node("b", "other", &["a"])]),
            Err(PlanGraphError::CrossMission("b".into(), "other".into()))
        );
        assert_eq!(
            validate_plan_graph(&[node("a", "m", &[]), node("b", "m", &["a", "a"])]),
            Err(PlanGraphError::DuplicateEdge("b".into(), "a".into()))
        );
    }
}
