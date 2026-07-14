//! `autoreview rules mine` — the first real (non-stub) piece of the M3 rule
//! factory: clusters recorded agent findings into candidate seeds and
//! writes them to `.autoreview/rules/candidates/<clusterId>/seed.json` for
//! the (not-yet-built) draft stage to pick up. Bench/review/shadow-log/
//! rollback stay stubs (`run_rules_stub`) until their own infrastructure
//! lands.

use autoreview_core::{mine_candidates, write_seed_file, HistoryStore};

use super::history::history_dir_for;

pub fn run_rules_mine(repo_root: &std::path::Path) -> anyhow::Result<()> {
    let history_dir = history_dir_for(repo_root);
    let store = HistoryStore::open(&history_dir)?;
    let findings = store.agent_findings_for_mining()?;

    if findings.is_empty() {
        println!("No agent findings recorded yet on this machine — nothing to mine. Run `autoreview diff` a few times first.");
        return Ok(());
    }

    let seeds = mine_candidates(findings);
    if seeds.is_empty() {
        println!("No recurring clusters found (need >= 3 similar findings spanning >= 2 distinct runs).");
        return Ok(());
    }

    println!("Found {} candidate cluster(s):", seeds.len());
    for seed in &seeds {
        let path = write_seed_file(repo_root, seed)?;
        println!(
            "  {} ({}, {} member(s) across {} run(s)) -> {}",
            seed.cluster_id,
            seed.category,
            seed.member_fingerprints.len(),
            seed.distinct_run_count,
            path.display()
        );
    }
    println!("\n(Draft/bench/shadow/promote are not yet implemented — these seeds are ready for that pipeline once it lands.)");
    Ok(())
}
