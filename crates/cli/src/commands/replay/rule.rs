use std::path::Path;
use std::process::Command;

use anyhow::ensure;
use autoreview_schema::AgentFinding;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(super) enum Tool {
    AstGrep,
    AutoreviewArchgraph,
    AutoreviewComplexity,
    AutoreviewDataflow,
    AutoreviewDuplication,
    AutoreviewFrameworks,
    AutoreviewRust,
}

impl Tool {
    pub(super) fn scan(self, source: &Path, paths: &[String]) -> anyhow::Result<Vec<AgentFinding>> {
        let findings = match self {
            Self::AstGrep => {
                ensure!(
                    Command::new("ast-grep")
                        .arg("--version")
                        .output()
                        .is_ok_and(|output| output.status.success()),
                    "ast-grep is required for this rule"
                );
                autoreview_core::run_ast_grep(source, paths, &[])?
            }
            Self::AutoreviewComplexity => autoreview_core::run_complexity_check(source, paths, &[]),
            Self::AutoreviewArchgraph => autoreview_core::run_archgraph_check(source, paths),
            Self::AutoreviewDataflow => autoreview_core::run_dataflow_check(source, paths, &[]),
            Self::AutoreviewDuplication => autoreview_core::run_duplication_check(source, paths),
            Self::AutoreviewFrameworks => autoreview_core::run_framework_checks(source, paths),
            Self::AutoreviewRust => autoreview_core::run_rust_review(source, paths),
        };
        Ok(findings)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Rule {
    pub(super) tool: Tool,
    pub(super) id: String,
}

impl Rule {
    pub(super) fn matches(&self, finding: &AgentFinding) -> bool {
        finding.source.rule_id.as_deref() == Some(&self.id)
    }

    pub(super) fn check_replay_support(&self) -> anyhow::Result<()> {
        let definition = autoreview_core::find_rule_definition(&self.id, &[]);
        let kind = definition.as_ref().map(|rule| rule.kind.as_str());
        let supported = match self.tool {
            Tool::AstGrep => kind == Some("pattern"),
            Tool::AutoreviewArchgraph => {
                matches!(self.id.as_str(), "import-cycle" | "web-import-cycle")
            }
            Tool::AutoreviewComplexity => kind == Some("threshold"),
            Tool::AutoreviewDataflow => {
                matches!(kind, Some("taint" | "call-sequence"))
                    || matches!(
                        self.id.as_str(),
                        "typed-nil-interface-return"
                            | "append-shared-backing-array"
                            | "npe-risk-from-helper-return"
                            | "loopvar-capture-pre-1.22"
                            | "loopvar-address-pre-1.22"
                    )
            }
            Tool::AutoreviewDuplication => self.id == "duplicate-code-block",
            Tool::AutoreviewFrameworks => kind == Some("framework"),
            Tool::AutoreviewRust => autoreview_core::rust_review_rule(&self.id).is_some(),
        };
        ensure!(
            supported,
            "rule '{}' is unavailable for replay with {:?}",
            self.id,
            self.tool
        );
        Ok(())
    }
}
