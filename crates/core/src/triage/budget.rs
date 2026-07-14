//! Cost-ceiling enforcement: `--max-usd`/`budgets` config was previously
//! only recorded in the report, never actually checked mid-run — a diff
//! could blow straight past its stated ceiling since nothing looked at
//! `total_usd` while specialists were still being launched. This is the
//! pure decision logic (fail-open when cost is unknown, since an
//! unmeasurable ceiling can't be enforced honestly); the actual
//! stop-launching-more-specialists loop lives in `commands::diff` since it
//! needs to interleave with real spawns.

/// Whether the run should stop launching further specialists/stages.
/// Fails open (`false`) when `max_usd` isn't set, or when no backend has
/// ever reported a real dollar figure yet (`any_usd_reported == false`) —
/// a cost ceiling can't be honestly enforced against a number nothing has
/// measured, so quiet non-enforcement beats a false "over budget" stop
/// from a stale `0.0`.
pub fn should_stop_for_budget(total_usd_spent: f64, any_usd_reported: bool, max_usd: Option<f64>) -> bool {
    match max_usd {
        Some(max_usd) => any_usd_reported && total_usd_spent >= max_usd,
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn does_not_stop_when_no_ceiling_is_set() {
        assert!(!should_stop_for_budget(100.0, true, None));
    }

    #[test]
    fn stops_once_spend_reaches_the_ceiling() {
        assert!(should_stop_for_budget(1.0, true, Some(1.0)));
    }

    #[test]
    fn does_not_stop_below_the_ceiling() {
        assert!(!should_stop_for_budget(0.5, true, Some(1.0)));
    }

    #[test]
    fn fails_open_when_no_backend_has_reported_a_real_cost_yet() {
        assert!(!should_stop_for_budget(0.0, false, Some(1.0)), "unmeasured cost can't be honestly compared to a ceiling");
    }
}
