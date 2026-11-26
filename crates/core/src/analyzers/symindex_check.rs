//! Turns `autoreview-symindex`'s whole-repo symbol index into reportable
//! findings — the crate itself has no knowledge of the schema's `Finding`
//! type, per its own module docs, so that mapping lives here (same
//! separation as `archgraph_check.rs` vs `autoreview-archgraph`).
//!
//! Phase 3 wired Message Chains (the smallest slice, to validate the whole
//! plumbing first); Phase 4 added Feature Envy; Phase 5 adds Data Clumps —
//! all three on the same bail-empty/whole-repo-build/diff-scope-filter
//! shape.
//!
//! Deliberately diff-relevant, not a whole-repo audit dump: the index is
//! built over the *entire* repo (per the plan's whole-repo-always scoping
//! decision — Feature Envy inherently needs to see the envied type's own
//! file, not just the envious method's file), but this only reports a
//! result involving a file the current diff touched.

use std::collections::HashSet;
use std::path::Path;

use autoreview_schema::{AgentFinding, FindingSource, FindingSourceKind, Location, LocationRange, Severity, Side};
use autoreview_symindex::SymbolIndex;

/// Fowler's own illustrative example (`a.b().c().d()`) is depth 3 — that's
/// the threshold, not an arbitrarily higher bar, since a shorter chain is
/// often unremarkable delegation.
const MIN_CHAIN_DEPTH: usize = 3;

/// A method needs at least this many accesses to a single foreign type
/// before Feature Envy is even considered — below this, it's ordinary,
/// unremarkable collaboration, not envy.
const MIN_FOREIGN_ACCESSES: usize = 3;
/// ...and that count must exceed the method's own-field access count by at
/// least this much, so a method that's genuinely balanced between its own
/// state and a collaborator's doesn't trip the check.
const FEATURE_ENVY_MARGIN: i64 = 2;

/// A recurring parameter group needs at least this many params before it's
/// distinctive enough to be worth flagging (a 1- or 2-param match is too
/// common to be meaningful) and must recur across at least this many
/// distinct methods.
const MIN_CLUMP_LEN: usize = 3;
const MIN_CLUMP_METHODS: usize = 3;

fn path_str(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

const RELEVANT_EXTENSIONS: &[&str] = &[".go", ".java", ".kt", ".kts", ".ts", ".mts", ".cts", ".tsx", ".js", ".jsx", ".mjs", ".cjs"];

/// Runs the symbol-index queries over the whole repo and reports only
/// results whose caller or contributing helper the current diff touched. Returns an empty
/// list (not an error) when the diff touches no file in a language
/// `autoreview-symindex` extracts from (see `RELEVANT_EXTENSIONS`).
pub fn run_symindex_check(repo_root: &Path, changed_files: &[String]) -> Vec<AgentFinding> {
    run_symindex_check_with_tier4(repo_root, changed_files, false)
}

/// Same as `run_symindex_check`, plus an opt-in Tier 4 real-semantic pass
/// (see SESSION_NOTES.md follow-up #3 / `autoreview_symindex::tier4`'s own
/// docs) over Feature Envy findings when `tier4_go_enabled` is set: shells
/// out to the companion Go tool to type-check the repo for real, then
/// annotates each Feature Envy finding as type-confirmed, contradicted (and
/// dropped — a real type-checker disagreeing with the heuristic is strong
/// enough evidence to suppress it outright, not just soften it), or left
/// alone when no Tier 4 data is available for that method. Fails soft: if
/// the tool run produces no data at all (no `go` on `PATH`, non-Go repo,
/// build failure), this is identical to the flag being off.
pub fn run_symindex_check_with_tier4(repo_root: &Path, changed_files: &[String], tier4_go_enabled: bool) -> Vec<AgentFinding> {
    let relevant_changed: Vec<&String> = changed_files.iter().filter(|f| RELEVANT_EXTENSIONS.iter().any(|ext| f.ends_with(ext))).collect();
    if relevant_changed.is_empty() {
        return Vec::new();
    }
    let changed_set: HashSet<&String> = relevant_changed.into_iter().collect();

    let index = autoreview_symindex::build_index(repo_root);

    let chains = autoreview_symindex::find_message_chains(&index, MIN_CHAIN_DEPTH);
    let envy = autoreview_symindex::find_feature_envy(&index, MIN_FOREIGN_ACCESSES, FEATURE_ENVY_MARGIN);
    let clumps = autoreview_symindex::find_data_clumps(&index, MIN_CLUMP_LEN, MIN_CLUMP_METHODS, autoreview_symindex::ClumpScope::WholeIndex);
    let refused_bequests = autoreview_symindex::find_refused_bequest(&index);

    let tier4_records = if tier4_go_enabled { autoreview_symindex::run_tier4_go(repo_root) } else { None };

    let mut findings: Vec<AgentFinding> = chains.into_iter().filter(|c| changed_set.contains(&path_str(&c.file))).map(chain_to_finding).collect();
    findings.extend(
        envy.into_iter()
            .filter(|e| changed_set.contains(&path_str(&e.file))
                || e.helper_locations().any(|helper| changed_set.contains(&path_str(&helper.file))))
            .filter_map(|e| envy_to_finding_with_tier4(&index, e, tier4_records.as_deref())),
    );
    findings.extend(clumps.iter().filter_map(|c| clump_to_finding(c, &changed_set)));
    findings.extend(refused_bequests.into_iter().filter(|r| changed_set.contains(&path_str(&r.file))).map(refused_bequest_to_finding));
    findings
}

fn chain_to_finding(chain: autoreview_symindex::ChainFinding) -> AgentFinding {
    let path = path_str(&chain.file);
    let chain_text = chain.chain_text.clone();
    AgentFinding {
        source: FindingSource { kind: FindingSourceKind::Analyzer, tool: "autoreview-symindex".to_string(), rule_id: Some("message-chain".to_string()), aspect: None, backend: None },
        category: "design".to_string(),
        severity: Severity::Low,
        confidence: 1.0,
        title: "Long message chain".to_string(),
        message: format!(
            "`{chain_text}` in `{}.{}` chains {} calls deep off a single expression. Each link couples this code to the internal structure of every intermediate object (Fowler's Message Chains smell / Law of Demeter) — consider adding a method on the immediate collaborator that hides the chain. Heuristic, name-based match — not resolved against imports/types, so a coincidentally similar-looking chain could be a false positive.",
            chain.owner_type, chain.method, chain.depth
        ),
        location: Location { path, range: LocationRange { start_line: chain.line, ..Default::default() }, snippet: chain_text, side: Side::New },
        related_locations: None,
        suggestion: None,
        tags: None,
        meta: None,
        suggested_patch: None,
    }
}

/// Populates `related_locations` with the envied type's own declaration
/// site (when it's found elsewhere in the index) — a deliberate
/// improvement over `archgraph_check.rs`'s own precedent of only naming
/// related locations in message-text prose, since `AgentFinding` already
/// has a structured field for exactly this.
/// Tier 4-aware wrapper around Feature Envy's finding conversion. Returns
/// `None` when Tier 4 data is available for this method and it flatly
/// contradicts the heuristic — real type information is strong enough
/// evidence to drop the finding rather than merely flag it as unconfirmed.
fn envy_to_finding_with_tier4(index: &SymbolIndex, envy: autoreview_symindex::FeatureEnvyFinding, tier4_records: Option<&[autoreview_symindex::Tier4Access]>) -> Option<AgentFinding> {
    // The suggested refactor moves code to the collaborator. Require a
    // declaration we can point to in the repository.
    if envy.helper_evidence.is_empty() {
        index.find_type(&envy.envied_type)?;
    }
    let verdict = tier4_records.map(|records| autoreview_symindex::confirm_feature_envy(records, &envy));
    if verdict == Some(autoreview_symindex::Tier4Verdict::Contradicted) {
        return None;
    }
    Some(envy_to_finding(index, envy, verdict))
}

fn envy_to_finding(index: &SymbolIndex, envy: autoreview_symindex::FeatureEnvyFinding, tier4_verdict: Option<autoreview_symindex::Tier4Verdict>) -> AgentFinding {
    let path = path_str(&envy.file);
    let related_locations = if let Some(first) = envy.helper_evidence.first() {
        let mut locations = vec![Location {
            path: path_str(&first.type_file),
            range: LocationRange { start_line: first.type_line, ..Default::default() },
            snippet: envy.envied_type.clone(),
            side: Side::New,
        }];
        let mut seen = HashSet::new();
        for helper in envy.helper_locations() {
            if seen.insert(helper) {
                locations.push(Location {
                    path: path_str(&helper.file),
                    range: LocationRange { start_line: helper.start_line, end_line: Some(helper.end_line), ..Default::default() },
                    snippet: "Cross-file helper".to_string(),
                    side: Side::New,
                });
            }
        }
        Some(locations)
    } else {
        index.find_type(&envy.envied_type).map(|t| {
            vec![Location {
                path: path_str(&t.file),
                range: LocationRange { start_line: t.start_line, ..Default::default() },
                snippet: envy.envied_type.clone(),
                side: Side::New,
            }]
        })
    };
    let (access_scope, heuristic_caveat) = if envy.helper_evidence.is_empty() {
        ("via a parameter", "This heuristic counts direct parameter accesses and matches the collaborator by name. Confirm the related declaration before moving the method.")
    } else {
        ("including accesses in cross-file helpers", "This heuristic follows parameter bindings within the Go package. Check the linked helpers and collaborator declaration before moving the method.")
    };
    let (message_suffix, confidence) = match tier4_verdict {
        Some(autoreview_symindex::Tier4Verdict::Confirmed) => (
            " Confirmed by real type-checking (Tier 4: golang.org/x/tools/go/packages) — this isn't just a name-based guess.".to_string(),
            1.0,
        ),
        _ => (format!(" {heuristic_caveat}"), 0.6),
    };
    AgentFinding {
        source: FindingSource { kind: FindingSourceKind::Analyzer, tool: "autoreview-symindex".to_string(), rule_id: Some("feature-envy".to_string()), aspect: None, backend: None },
        category: "design".to_string(),
        severity: Severity::Low,
        confidence,
        title: "Possible Feature Envy".to_string(),
        message: format!(
            "`{}.{}` accesses `{}` {} times ({access_scope}) versus {} access(es) to its own fields — it may belong on `{}` instead (Fowler's Feature Envy smell).{message_suffix}",
            envy.owner_type, envy.method, envy.envied_type, envy.envied_access_count, envy.own_access_count, envy.envied_type
        ),
        location: Location { path, range: LocationRange { start_line: envy.line, ..Default::default() }, snippet: format!("{}.{}", envy.owner_type, envy.method), side: Side::New },
        related_locations,
        suggestion: None,
        tags: None,
        meta: None,
        suggested_patch: None,
    }
}

/// A data clump has no single "file" (it spans multiple methods, often
/// multiple files) — anchors the finding at whichever member method
/// actually lives in a changed file (mirroring `archgraph_check.rs`'s own
/// anchor-at-a-changed-file convention for a multi-location result), and
/// lists every other member as a `related_locations` entry. Returns `None`
/// if no member touches a changed file at all (nothing to anchor on).
fn clump_to_finding(clump: &autoreview_symindex::DataClumpFinding, changed_set: &HashSet<&String>) -> Option<AgentFinding> {
    let anchor = clump.methods.iter().find(|m| changed_set.contains(&path_str(&m.file)))?;

    let related_locations: Vec<Location> = clump
        .methods
        .iter()
        .filter(|m| !(m.owner_type == anchor.owner_type && m.method == anchor.method && m.file == anchor.file))
        .map(|m| Location { path: path_str(&m.file), range: LocationRange { start_line: m.line, ..Default::default() }, snippet: format!("{}.{}", m.owner_type, m.method), side: Side::New })
        .collect();

    let signature_text = clump.signature.iter().map(|s| format!("{}: {}", s.name, s.type_text)).collect::<Vec<_>>().join(", ");
    let methods_text = clump.methods.iter().map(|m| format!("{}.{}", m.owner_type, m.method)).collect::<Vec<_>>().join(", ");

    Some(AgentFinding {
        source: FindingSource { kind: FindingSourceKind::Analyzer, tool: "autoreview-symindex".to_string(), rule_id: Some("data-clump".to_string()), aspect: None, backend: None },
        category: "design".to_string(),
        severity: Severity::Low,
        confidence: 1.0,
        title: "Recurring parameter group (Data Clump)".to_string(),
        message: format!(
            "The parameter group ({signature_text}) appears identically across {} methods: {methods_text}. Consider consolidating these into their own small object (Fowler's Data Clumps smell). Heuristic, name-based match — parameter types aren't resolved against imports, so a coincidentally identical group in genuinely unrelated code could be a false positive.",
            clump.methods.len()
        ),
        location: Location { path: path_str(&anchor.file), range: LocationRange { start_line: anchor.line, ..Default::default() }, snippet: signature_text, side: Side::New },
        related_locations: if related_locations.is_empty() { None } else { Some(related_locations) },
        suggestion: None,
        tags: None,
        meta: None,
        suggested_patch: None,
    })
}

fn refused_bequest_to_finding(rb: autoreview_symindex::RefusedBequestFinding) -> AgentFinding {
    let path = path_str(&rb.file);
    AgentFinding {
        source: FindingSource { kind: FindingSourceKind::Analyzer, tool: "autoreview-symindex".to_string(), rule_id: Some("refused-bequest".to_string()), aspect: None, backend: None },
        category: "design".to_string(),
        severity: Severity::Low,
        confidence: 1.0,
        title: "Possible Refused Bequest".to_string(),
        message: format!(
            "`{}.{}` overrides `{}.{}` but its body is empty or just throws UnsupportedOperationException/NotImplementedError — this subclass refuses part of its parent's contract (Fowler's Refused Bequest smell). Consider whether `{}` should really extend `{}`, or whether the parent method belongs on a narrower interface `{}` can actually fulfill.",
            rb.owner_type, rb.method, rb.superclass, rb.method, rb.owner_type, rb.superclass, rb.owner_type
        ),
        location: Location { path, range: LocationRange { start_line: rb.line, ..Default::default() }, snippet: format!("{}.{}", rb.owner_type, rb.method), side: Side::New },
        related_locations: None,
        suggestion: None,
        tags: None,
        meta: None,
        suggested_patch: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_file(dir: &Path, rel_path: &str, content: &str) {
        let path = dir.join(rel_path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    #[test]
    fn returns_empty_when_no_relevant_files_changed() {
        let dir = tempfile::tempdir().unwrap();
        write_file(dir.path(), "main.go", "package main\n\nfunc main() {}\n");
        let findings = run_symindex_check(dir.path(), &["README.md".to_string()]);
        assert!(findings.is_empty());
    }

    #[test]
    fn reports_a_message_chain_that_touches_a_changed_file() {
        let dir = tempfile::tempdir().unwrap();
        write_file(
            dir.path(),
            "Widget.java",
            "class Widget {\n    Owner owner;\n    String chain() {\n        return owner.getAddress().getCity().toUpperCase();\n    }\n}\n",
        );
        let findings = run_symindex_check(dir.path(), &["Widget.java".to_string()]);
        assert_eq!(findings.len(), 1, "got: {findings:#?}");
        assert_eq!(findings[0].category, "design");
        assert_eq!(findings[0].source.rule_id.as_deref(), Some("message-chain"));
        assert!(findings[0].message.contains("3 calls deep"));
    }

    #[test]
    fn does_not_report_a_chain_the_diff_never_touches() {
        let dir = tempfile::tempdir().unwrap();
        write_file(
            dir.path(),
            "Widget.java",
            "class Widget {\n    Owner owner;\n    String chain() {\n        return owner.getAddress().getCity().toUpperCase();\n    }\n}\n",
        );
        write_file(dir.path(), "Unrelated.java", "class Unrelated {\n    void f() {}\n}\n");
        let findings = run_symindex_check(dir.path(), &["Unrelated.java".to_string()]);
        assert!(findings.is_empty());
    }

    #[test]
    fn does_not_report_a_chain_below_the_depth_threshold() {
        let dir = tempfile::tempdir().unwrap();
        write_file(dir.path(), "Widget.java", "class Widget {\n    Owner owner;\n    String f() {\n        return owner.getAddress().getCity();\n    }\n}\n");
        let findings = run_symindex_check(dir.path(), &["Widget.java".to_string()]);
        assert!(findings.is_empty());
    }

    #[test]
    fn reports_a_go_message_chain_that_touches_a_changed_file() {
        let dir = tempfile::tempdir().unwrap();
        write_file(dir.path(), "widget.go", "package main\n\ntype Widget struct{}\n\nfunc (w *Widget) Chain() string {\n\treturn w.Sub().GetCity().ToUpperCase()\n}\n");
        let findings = run_symindex_check(dir.path(), &["widget.go".to_string()]);
        assert_eq!(findings.len(), 1, "got: {findings:#?}");
    }

    #[test]
    fn reports_a_cross_file_feature_envy_that_touches_a_changed_file() {
        let dir = tempfile::tempdir().unwrap();
        write_file(
            dir.path(),
            "Widget.java",
            "class Widget {\n    int quantity;\n    int total(Customer c) {\n        int a = c.getBalance();\n        int b = c.getFee();\n        int d = c.getTax();\n        return a + b + d;\n    }\n}\n",
        );
        write_file(dir.path(), "Customer.java", "class Customer {\n    int balance;\n}\n");

        let findings = run_symindex_check(dir.path(), &["Widget.java".to_string()]);
        let envy: Vec<_> = findings.iter().filter(|f| f.source.rule_id.as_deref() == Some("feature-envy")).collect();
        assert_eq!(envy.len(), 1, "got: {findings:#?}");
        assert_eq!(envy[0].category, "design");
        assert!(envy[0].message.contains("Customer"));
        // The envied type's own declaration lives in a *different* file
        // than the one the diff touched — proving the index really is
        // built whole-repo, not scoped to changed_files, while the
        // *reporting* still only fires because Widget.java (the envious
        // method's own file) is in the diff.
        let related = envy[0].related_locations.as_ref().expect("expected related_locations to be populated");
        assert_eq!(related[0].path, "Customer.java");
    }

    #[test]
    fn reports_go_feature_envy_through_a_cross_file_helper() {
        let dir = tempfile::tempdir().unwrap();
        write_file(dir.path(), "billing/invoice.go", r#"
package billing
type Invoice struct{}
func (i Invoice) Total(other, customer *Customer) int {
    alias := customer
    return amounts(other, alias)
}
func (i Invoice) Taxed(customer *Customer) int {
    return costs(customer) + customer.Tax
}
"#);
        write_file(dir.path(), "billing/customer.go", "package billing\ntype Customer struct { Balance, Fee, Tax int }\n");
        write_file(dir.path(), "billing/amounts.go", r#"
package billing
func amounts(unused, customer *Customer) int {
    alias := customer
    return alias.Balance + alias.Fee + alias.Tax
}
func costs(customer *Customer) int { return customer.Balance + customer.Fee }
"#);
        write_file(dir.path(), "unrelated/customer.go", "package billing\ntype Customer struct{}\n");

        let findings = run_symindex_check(dir.path(), &["billing/invoice.go".to_string()]);
        let envy: Vec<_> = findings.iter().filter(|f| f.source.rule_id.as_deref() == Some("feature-envy")).collect();
        assert_eq!(envy.len(), 2, "got: {findings:#?}");
        for finding in &envy {
            assert!(finding.message.contains("`Customer` 3 times"));
            let related = finding.related_locations.as_ref().unwrap();
            assert_eq!(related[0].path, "billing/customer.go");
            assert_eq!(related[1].path, "billing/amounts.go");
        }
        assert_eq!(envy[0].location.path, "billing/invoice.go");
        assert_eq!(envy[0].location.range.start_line, 4);
        assert_eq!(envy[0].related_locations.as_ref().unwrap()[1].range.end_line, Some(6));

        let helper_only = run_symindex_check(dir.path(), &["billing/amounts.go".to_string()]);
        assert_eq!(helper_only.iter().filter(|finding| finding.source.rule_id.as_deref() == Some("feature-envy")).count(), 2);
        let unrelated = run_symindex_check(dir.path(), &["unrelated/customer.go".to_string()]);
        assert!(unrelated.iter().all(|finding| finding.source.rule_id.as_deref() != Some("feature-envy")));
    }

    #[test]
    fn reports_go_feature_envy_two_cross_file_hops_away() {
        let dir = tempfile::tempdir().unwrap();
        write_file(dir.path(), "invoice.go", r#"
package billing
type Invoice struct{}
type Customer struct { Balance, Fee, Tax int }
func (Invoice) Total(customer *Customer) int {
    alias := customer
    return forward(0, alias) + relay(alias)
}
func (Invoice) DirectAndIndirect(customer *Customer) int {
    return totals(customer, 0) + relay(customer)
}
"#);
        write_file(dir.path(), "forward.go", r#"
package billing
func forward(fee int, customer *Customer) int {
    alias := customer
    return totals(alias, fee)
}
"#);
        write_file(dir.path(), "relay.go", "package billing\nfunc relay(c *Customer) int { return totals(c, 0) }\n");
        write_file(dir.path(), "totals.go", r#"
package billing
func totals(customer *Customer, fee int) int {
    return customer.Balance + customer.Fee + customer.Tax + fee
}
"#);

        let findings = run_symindex_check(dir.path(), &["invoice.go".to_string()]);
        let envy: Vec<_> = findings.iter().filter(|finding| finding.source.rule_id.as_deref() == Some("feature-envy")).collect();
        assert_eq!(envy.len(), 2, "got: {findings:#?}");
        assert!(envy.iter().all(|finding| finding.message.contains("`Customer` 3 times")));
        let related = envy[0].related_locations.as_ref().unwrap();
        let paths: HashSet<_> = related.iter().map(|location| location.path.as_str()).collect();
        assert_eq!(paths, HashSet::from(["invoice.go", "forward.go", "relay.go", "totals.go"]));
        for (file, expected) in [("forward.go", 1), ("relay.go", 2), ("totals.go", 2)] {
            let findings = run_symindex_check(dir.path(), &[file.to_string()]);
            assert_eq!(findings.iter().filter(|finding| finding.source.rule_id.as_deref() == Some("feature-envy")).count(), expected);
        }
    }

    #[test]
    fn skips_two_hop_ambiguity_and_ordinary_forwarding() {
        let dir = tempfile::tempdir().unwrap();
        write_file(dir.path(), "invoice.go", r#"
package billing
type Invoice struct{}
type Customer struct { Balance, Fee, Tax int }
func (c *Customer) Total() int { return c.Balance + c.Fee + c.Tax }
func (Invoice) Ambiguous(c *Customer) int { return uncertain(c) }
func (Invoice) Shadowed(f func(*Customer) int, c *Customer) int { return shadowed(c, f) }
func (Invoice) Forward(c *Customer) int { return forwarding(c) + forwarding(c) }
func (Invoice) WrongSlot(c *Customer) int { return wrongSlot(c, load()) }
func (Invoice) Repeat(c *Customer) int { return repeated(c) }
func (Invoice) TooFar(c *Customer) int { return threeHops(c) }
func (Invoice) SameFile(c *Customer) int { return sameFile(c) }
"#);
        write_file(dir.path(), "forward.go", r#"
package billing
func uncertain(c *Customer) int { return platform(c) }
func shadowed(c *Customer, wide func(*Customer) int) int { return wide(c) }
func forwarding(c *Customer) int { return onlyTotal(c) }
func wrongSlot(unused, other *Customer) int { return wide(other) }
func repeated(c *Customer) int { return two(c) + two(c) }
func threeHops(c *Customer) int { return relay(c) }
func sameFile(c *Customer) int { return sameFileTarget(c) }
func sameFileTarget(c *Customer) int { return c.Balance + c.Fee + c.Tax }
"#);
        write_file(dir.path(), "leaf.go", r#"
package billing
func onlyTotal(c *Customer) int { return c.Total() }
func two(c *Customer) int { return c.Balance + c.Fee }
func wide(c *Customer) int { return c.Balance + c.Fee + c.Tax }
func relay(c *Customer) int { return sameFileTarget(c) }
func load() *Customer { return nil }
"#);
        write_file(dir.path(), "platform_linux.go", "package billing\nfunc platform(c *Customer) int { return c.Balance + c.Fee + c.Tax }\n");
        write_file(dir.path(), "platform_windows.go", "package billing\nvar platform = onlyTotal\n");

        let findings = run_symindex_check(dir.path(), &["invoice.go".to_string()]);
        assert!(findings.iter().all(|finding| finding.source.rule_id.as_deref() != Some("feature-envy")), "got: {findings:#?}");
    }

    #[test]
    fn skips_go_helper_forwarding_and_ambiguous_bindings() {
        let dir = tempfile::tempdir().unwrap();
        write_file(dir.path(), "invoice.go", r#"
package billing
type Invoice struct{}
type Customer struct { Balance, Fee, Tax int }
func (c *Customer) Total() int { return c.Balance + c.Fee + c.Tax }
func (i Invoice) Forward(customer *Customer) int {
    return forward(customer) + forward(customer) + forward(customer)
}
func (i Invoice) Shadow(wide func(*Customer) int, customer *Customer) int {
    return wide(customer)
}
func (i Invoice) Reassigned(customer, other *Customer) int {
    alias := customer
    alias = other
    return wide(alias)
}
func (i Invoice) WrongSlot(customer *Customer) int {
    return wideSecond(customer, loadCustomer())
}
func (i Invoice) Repeat(customer *Customer) int {
    return two(customer) + two(customer)
}
func (i Invoice) Ambiguous(customer *Customer) int { return duplicate(customer) }
func (i Invoice) FunctionValue(customer *Customer) int { return platform(customer) }
"#);
        write_file(dir.path(), "helpers.go", r#"
package billing
func forward(customer *Customer) int { return customer.Total() }
func wide(customer *Customer) int { return customer.Balance + customer.Fee + customer.Tax }
func wideSecond(unused, customer *Customer) int { return customer.Balance + customer.Fee + customer.Tax }
func loadCustomer() *Customer { return nil }
func two(customer *Customer) int { return customer.Balance + customer.Fee }
"#);
        let duplicate = "package billing\nfunc duplicate(c *Customer) int { return c.Balance + c.Fee + c.Tax }\n";
        write_file(dir.path(), "duplicate_windows.go", duplicate);
        write_file(dir.path(), "duplicate_linux.go", duplicate);
        write_file(dir.path(), "platform_linux.go", "package billing\nfunc platform(c *Customer) int { return c.Balance + c.Fee + c.Tax }\n");
        write_file(dir.path(), "platform_windows.go", "package billing\nvar (\n platform = forward\n)\n");

        let findings = run_symindex_check(dir.path(), &["invoice.go".to_string()]);
        assert!(findings.iter().all(|f| f.source.rule_id.as_deref() != Some("feature-envy")), "got: {findings:#?}");
    }

    #[test]
    fn does_not_report_feature_envy_below_the_threshold() {
        let dir = tempfile::tempdir().unwrap();
        write_file(dir.path(), "Widget.java", "class Widget {\n    int total(Customer c) {\n        return c.getBalance();\n    }\n}\n");
        let findings = run_symindex_check(dir.path(), &["Widget.java".to_string()]);
        assert!(findings.iter().all(|f| f.source.rule_id.as_deref() != Some("feature-envy")), "got: {findings:#?}");
    }

    #[test]
    fn does_not_report_feature_envy_the_diff_never_touches() {
        let dir = tempfile::tempdir().unwrap();
        write_file(
            dir.path(),
            "Widget.java",
            "class Widget {\n    int total(Customer c) {\n        int a = c.getBalance();\n        int b = c.getFee();\n        int d = c.getTax();\n        return a + b + d;\n    }\n}\n",
        );
        write_file(dir.path(), "Unrelated.java", "class Unrelated {\n    void f() {}\n}\n");
        let findings = run_symindex_check(dir.path(), &["Unrelated.java".to_string()]);
        assert!(findings.iter().all(|f| f.source.rule_id.as_deref() != Some("feature-envy")));
    }

    #[test]
    fn reports_a_data_clump_recurring_across_three_files_anchored_at_the_changed_one() {
        let dir = tempfile::tempdir().unwrap();
        let sig = "String name, int id, boolean active";
        write_file(dir.path(), "A.java", &format!("class A {{\n    void one({sig}) {{}}\n}}\n"));
        write_file(dir.path(), "B.java", &format!("class B {{\n    void two({sig}) {{}}\n}}\n"));
        write_file(dir.path(), "C.java", &format!("class C {{\n    void three({sig}) {{}}\n}}\n"));

        let findings = run_symindex_check(dir.path(), &["A.java".to_string()]);
        let clumps: Vec<_> = findings.iter().filter(|f| f.source.rule_id.as_deref() == Some("data-clump")).collect();
        assert_eq!(clumps.len(), 1, "got: {findings:#?}");
        assert_eq!(clumps[0].location.path, "A.java");
        let related = clumps[0].related_locations.as_ref().expect("expected related_locations");
        assert_eq!(related.len(), 2, "expected B and C as related locations, got: {related:#?}");
    }

    #[test]
    fn does_not_report_a_data_clump_the_diff_never_touches() {
        let dir = tempfile::tempdir().unwrap();
        let sig = "String name, int id, boolean active";
        write_file(dir.path(), "A.java", &format!("class A {{\n    void one({sig}) {{}}\n}}\n"));
        write_file(dir.path(), "B.java", &format!("class B {{\n    void two({sig}) {{}}\n}}\n"));
        write_file(dir.path(), "C.java", &format!("class C {{\n    void three({sig}) {{}}\n}}\n"));
        write_file(dir.path(), "Unrelated.java", "class Unrelated {\n    void f() {}\n}\n");

        let findings = run_symindex_check(dir.path(), &["Unrelated.java".to_string()]);
        assert!(findings.iter().all(|f| f.source.rule_id.as_deref() != Some("data-clump")));
    }

    #[test]
    fn does_not_report_a_data_clump_below_the_min_methods_threshold() {
        let dir = tempfile::tempdir().unwrap();
        let sig = "String name, int id, boolean active";
        write_file(dir.path(), "A.java", &format!("class A {{\n    void one({sig}) {{}}\n}}\n"));
        write_file(dir.path(), "B.java", &format!("class B {{\n    void two({sig}) {{}}\n}}\n"));

        let findings = run_symindex_check(dir.path(), &["A.java".to_string()]);
        assert!(findings.iter().all(|f| f.source.rule_id.as_deref() != Some("data-clump")));
    }

    fn sample_envy() -> autoreview_symindex::FeatureEnvyFinding {
        autoreview_symindex::FeatureEnvyFinding {
            file: std::path::PathBuf::from("self.go"),
            line: 1,
            owner_type: "Self".to_string(),
            method: "Envious".to_string(),
            envied_type: "Other".to_string(),
            envied_access_count: 3,
            own_access_count: 0,
            helper_evidence: Vec::new(),
        }
    }

    fn sample_index() -> SymbolIndex {
        SymbolIndex { types: vec![autoreview_symindex::TypeDecl {
            name: "Other".to_string(), file: "other.go".into(), start_line: 1,
            fields: vec![], methods: vec![], superclass: None,
        }] }
    }

    #[test]
    fn envy_finding_carries_no_tier4_caveat_language_when_no_records_available() {
        let finding = envy_to_finding_with_tier4(&sample_index(), sample_envy(), None).unwrap();
        assert!(finding.confidence < 1.0);
        assert!(finding.message.contains("matches the collaborator by name"));
    }

    #[test]
    fn drops_feature_envy_when_the_collaborator_has_no_local_declaration() {
        assert!(envy_to_finding_with_tier4(&SymbolIndex::default(), sample_envy(), None).is_none());
    }

    #[test]
    fn envy_finding_is_marked_confirmed_when_tier4_agrees() {
        let records = vec![autoreview_symindex::Tier4Access {
            file: "self.go".to_string(),
            line: 1,
            method: "Envious".to_string(),
            receiver_type: "fixture.Self".to_string(),
            accessed_ident: "Get".to_string(),
            accessed_type: "fixture.Other".to_string(),
        }];
        let finding = envy_to_finding_with_tier4(&sample_index(), sample_envy(), Some(&records)).unwrap();
        assert!(finding.message.contains("Confirmed by real type-checking"));
    }

    #[test]
    fn envy_finding_is_dropped_when_tier4_contradicts_it() {
        let records = vec![autoreview_symindex::Tier4Access {
            file: "self.go".to_string(),
            line: 1,
            method: "Envious".to_string(),
            receiver_type: "fixture.Self".to_string(),
            accessed_ident: "Get".to_string(),
            accessed_type: "fixture.SomethingElse".to_string(),
        }];
        assert!(envy_to_finding_with_tier4(&sample_index(), sample_envy(), Some(&records)).is_none());
    }

    #[test]
    fn reports_a_refused_bequest_that_touches_a_changed_file() {
        let dir = tempfile::tempdir().unwrap();
        write_file(dir.path(), "Base.java", "class Base {\n    void save() {\n        System.out.println(\"saving\");\n    }\n}\n");
        write_file(dir.path(), "Sub.java", "class Sub extends Base {\n    void save() {\n        throw new UnsupportedOperationException();\n    }\n}\n");

        let findings = run_symindex_check(dir.path(), &["Sub.java".to_string()]);
        assert_eq!(findings.len(), 1, "got: {findings:#?}");
        assert_eq!(findings[0].source.rule_id.as_deref(), Some("refused-bequest"));
        assert_eq!(findings[0].category, "design");
    }

    #[test]
    fn does_not_report_a_refused_bequest_the_diff_never_touches() {
        let dir = tempfile::tempdir().unwrap();
        write_file(dir.path(), "Base.java", "class Base {\n    void save() {\n        System.out.println(\"saving\");\n    }\n}\n");
        write_file(dir.path(), "Sub.java", "class Sub extends Base {\n    void save() {\n        throw new UnsupportedOperationException();\n    }\n}\n");
        write_file(dir.path(), "Unrelated.java", "class Unrelated {\n    void f() {}\n}\n");

        let findings = run_symindex_check(dir.path(), &["Unrelated.java".to_string()]);
        assert!(findings.iter().all(|f| f.source.rule_id.as_deref() != Some("refused-bequest")));
    }

    #[test]
    fn reports_a_kotlin_message_chain_that_touches_a_changed_file() {
        let dir = tempfile::tempdir().unwrap();
        write_file(
            dir.path(),
            "Widget.kt",
            "class Widget {\n    var owner: Owner? = null\n    fun chain(): String {\n        return owner!!.getAddress().getCity().uppercase()\n    }\n}\n",
        );
        let findings = run_symindex_check(dir.path(), &["Widget.kt".to_string()]);
        assert!(findings.iter().any(|f| f.source.rule_id.as_deref() == Some("message-chain")), "got: {findings:#?}");
    }

    #[test]
    fn reports_kotlin_feature_envy_that_touches_a_changed_file() {
        let dir = tempfile::tempdir().unwrap();
        write_file(dir.path(), "Customer.kt", "class Customer {}\n");
        write_file(
            dir.path(),
            "Widget.kt",
            "class Widget {\n    fun envy(c: Customer): Int {\n        return c.getBalance() + c.getBalance() + c.getBalance()\n    }\n}\n",
        );
        let findings = run_symindex_check(dir.path(), &["Widget.kt".to_string()]);
        assert!(findings.iter().any(|f| f.source.rule_id.as_deref() == Some("feature-envy")), "got: {findings:#?}");
    }

    #[test]
    fn reports_typescript_feature_envy_that_touches_a_changed_file() {
        // Regression test: run_symindex_check_with_tier4's own
        // relevant-file filter used to hardcode go/java/kt/kts, so this
        // returned Vec::new() unconditionally for a TS-only diff even
        // though the underlying extractor/query layer worked fine.
        let dir = tempfile::tempdir().unwrap();
        write_file(dir.path(), "Customer.ts", "class Customer {}\n");
        write_file(
            dir.path(),
            "Widget.ts",
            "class Widget {\n    envy(c: Customer): number {\n        return c.getBalance() + c.getBalance() + c.getBalance();\n    }\n}\n",
        );
        let findings = run_symindex_check(dir.path(), &["Widget.ts".to_string()]);
        assert!(findings.iter().any(|f| f.source.rule_id.as_deref() == Some("feature-envy")), "got: {findings:#?}");
    }

    #[test]
    fn reports_a_kotlin_refused_bequest_that_touches_a_changed_file() {
        let dir = tempfile::tempdir().unwrap();
        write_file(dir.path(), "Base.kt", "class Base {\n    fun save() {\n        println(\"saving\")\n    }\n}\n");
        write_file(dir.path(), "Sub.kt", "class Sub : Base() {\n    fun save() {\n        TODO()\n    }\n}\n");

        let findings = run_symindex_check(dir.path(), &["Sub.kt".to_string()]);
        assert!(findings.iter().any(|f| f.source.rule_id.as_deref() == Some("refused-bequest")), "got: {findings:#?}");
    }
}
