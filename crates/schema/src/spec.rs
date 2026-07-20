//! Acceptance-criteria verification (Initiative 3, distilled from Aviator
//! Verify's spec-first model — see SESSION_NOTES.md): an optional
//! `.autoreview/spec.md` states what a change is *for* and a bullet list of
//! independently-verifiable claims it must satisfy, checked against the
//! diff by an LLM judge in addition to (not instead of) autoreview's
//! existing finding-based review. Deliberately the same three-field shape
//! Aviator's own spec format uses (title/intent/acceptance criteria) — no
//! reason to invent a different one.

use serde::{Deserialize, Serialize};

/// A parsed `.autoreview/spec.md`. See `parse_spec` (crate `core`) for the
/// exact markdown shape this is read from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcceptanceSpec {
    pub title: String,
    pub intent: String,
    pub criteria: Vec<String>,
}

/// `Uncertain` is a genuine third answer, not a fallback for "didn't check"
/// — the judge is explicitly instructed to prefer it over guessing when the
/// diff alone doesn't settle a criterion, mirroring Stage 3.5's own
/// default-to-not-refuting posture (see `crate::verify`'s prompt).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CriterionVerdict {
    Satisfied,
    NotSatisfied,
    Uncertain,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CriterionResult {
    pub criterion: String,
    pub verdict: CriterionVerdict,
    pub evidence: String,
}
