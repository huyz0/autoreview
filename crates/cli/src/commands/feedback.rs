//! `autoreview feedback` — records a verdict (or a missed-finding report)
//! into the local history store: the single entry point that will feed both
//! the rule factory and skill evolution (M3). Two shapes per the plan:
//! `feedback <finding-id> --fp|--tp|--doesnt-apply|--accepted-risk|--fix-in-followup
//! [--note]` looks up a finding this machine has previously seen and appends
//! feedback keyed to its fingerprint; `feedback <commit-sha> --missed
//! "<description>"` has no existing finding to look up (a miss is an
//! absence, not an event), so it's recorded keyed to the sha itself.

use autoreview_core::{append_event_log, feedback_event, fetch_embedding, load_config, EventRecord, HistoryStore};
use autoreview_schema::FeedbackVerdict;

use super::history::{history_dir_for, hostname};

pub fn run_missed_report(repo_root: &std::path::Path, id: &str, description: &str) -> anyhow::Result<()> {
    let history_dir = history_dir_for(repo_root);
    let host = hostname();
    let timestamp = chrono::Utc::now().to_rfc3339();

    let event = EventRecord {
        finding_fingerprint: id.to_string(),
        category: "unknown".to_string(),
        rule_id_or_aspect: "missed-report".to_string(),
        severity: "info".to_string(),
        feedback: Some(format!("missed: {description}")),
        run_id: "feedback".to_string(),
        host: host.clone(),
        timestamp: timestamp.clone(),
    };
    append_event_log(&history_dir, &chrono::Utc::now().format("%Y-%m-%d").to_string(), &host, std::slice::from_ref(&event))?;
    println!("Recorded missed-finding report for commit {id}: \"{description}\"");
    println!("(No matching finding to demote/reinforce — this feeds skill evolution's channel 3 in M3.)");
    Ok(())
}

pub fn run_feedback(repo_root: &std::path::Path, id: &str, verdict: FeedbackVerdict, note: Option<&str>) -> anyhow::Result<()> {
    let history_dir = history_dir_for(repo_root);
    let store = HistoryStore::open(&history_dir)?;
    let host = hostname();
    let timestamp = chrono::Utc::now().to_rfc3339();

    let lookup = match store.find_finding_by_id(id)? {
        Some(lookup) => lookup,
        None => {
            println!("No finding with id '{id}' was found in this repo's history at {}.", history_dir.display());
            println!("Feedback only works on a finding id printed by a previous `autoreview diff` run on this machine.");
            std::process::exit(1);
        }
    };

    store.record_feedback(id, &lookup, verdict, note, &timestamp)?;
    let event = feedback_event(&lookup, verdict.as_str(), "feedback", &host, &timestamp);
    append_event_log(&history_dir, &chrono::Utc::now().format("%Y-%m-%d").to_string(), &host, std::slice::from_ref(&event))?;

    // Best-effort: record an embedding of this finding's text for the Stage
    // 4 similarity filter, only if a repo has opted in and a server is
    // configured. Never fails the feedback command itself — a missing/down
    // embedding server shouldn't block recording a verdict.
    if let Ok(config) = load_config(&repo_root.join(".autoreview").join("config.yaml")) {
        if config.agents.embedding.enabled {
            let text = format!("{} {}", lookup.title, lookup.message);
            if let Ok(embedding) = fetch_embedding(&config.agents.embedding.base_url, &config.agents.embedding.model, &text, &config.agents.embedding.curl_binary) {
                let _ = store.record_embedding(&lookup.fingerprint, verdict, &embedding, &timestamp);
            }
        }
    }

    println!("Recorded '{}' feedback for finding {id} ({}: {}).", verdict.label(), lookup.category, lookup.rule_id_or_tool);
    if let Some(note) = note {
        println!("  note: \"{note}\"");
    }
    Ok(())
}
