//! Dollar admission uses immutable Run snapshots, not subscription percentages.
//! Reservations are estimates: providers can exceed them while already running.
use term_contracts::mission::types::{
    Binding, Policy, Run, RunDispatchState, UnknownCostPolicy, UsageCostSource,
};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CostSummary {
    /// Observed provider cost, including failed/cancelled/older attempts.
    pub observed_micros: u128,
    /// Unsettled reservations and estimates where final cost is unavailable.
    pub estimated_micros: u128,
    /// Runs whose contribution cannot be priced at all or has an unpriced tail.
    pub unknown_runs: usize,
}

impl CostSummary {
    pub fn committed_micros(&self) -> u128 {
        self.observed_micros + self.estimated_micros
    }
}

pub fn summarize_cost(runs: &[Run]) -> CostSummary {
    let mut total = CostSummary::default();
    for run in runs {
        let Some(binding) = &run.binding_snapshot else {
            continue;
        }; // deterministic work
           // Local termination reconciliation cannot settle an unknown provider bill.
        let owned = run.state.holds_execution_slot();
        if run.dispatch_state == RunDispatchState::Unsent && !owned {
            continue; // a cancelled, never-sent reservation costs nothing
        }
        let observed = run
            .usage
            .cost_usd_micros
            .as_ref()
            .filter(|_| run.usage.cost_source != UsageCostSource::Unknown)
            .map(|v| v.get() as u128);
        let estimate = binding
            .estimated_run_cost_usd_micros
            .as_ref()
            .map(|v| v.get() as u128);
        if let Some(cost) = observed {
            if run.usage.cost_source == UsageCostSource::Provider {
                total.observed_micros += cost;
            } else {
                total.estimated_micros += cost;
            }
        }
        if owned || observed.is_none() {
            match estimate {
                Some(estimate) => {
                    total.estimated_micros += estimate.saturating_sub(observed.unwrap_or(0))
                }
                None => total.unknown_runs += 1,
            }
        }
    }
    total
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CostBlock {
    Unknown,
    Limit,
}

impl CostBlock {
    pub fn code(self) -> &'static str {
        match self {
            Self::Unknown => "cost_unknown",
            Self::Limit => "cost_limit",
        }
    }
}

/// None binding is deterministic verification, with no provider-dollar cost.
pub fn cost_admission(
    policy: &Policy,
    runs: &[Run],
    binding: Option<&Binding>,
) -> Result<(), CostBlock> {
    let Some(binding) = binding else {
        return Ok(());
    };
    let total = summarize_cost(runs);
    let next = binding
        .estimated_run_cost_usd_micros
        .as_ref()
        .map(|v| v.get() as u128);
    if policy.unknown_cost == UnknownCostPolicy::Block && (next.is_none() || total.unknown_runs > 0)
    {
        return Err(CostBlock::Unknown);
    }
    if let Some(cap) = &policy.max_cost_usd_micros {
        let committed = total.committed_micros();
        if committed >= cap.get() as u128 || committed + next.unwrap_or(0) > cap.get() as u128 {
            return Err(CostBlock::Limit);
        }
    }
    Ok(())
}
