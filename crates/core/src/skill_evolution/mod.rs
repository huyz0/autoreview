//! Skill evolution (M3): mines feedback into proposed edits to a skill's
//! prompt (`instructions.md`, `examples.jsonl`, depth overlays), separate
//! from the rule factory since these are text edits for an LLM specialist
//! to read, not deterministic patterns. Per the plan's three input
//! channels, only channel 2 is implemented so far — repeated `--fp`
//! feedback with a human-supplied `--note`, clustered the same lexical way
//! as rule mining. Channels 1 (rule-drafting's own inexpressible verdicts)
//! and 3 (`--missed` reports, which only live in the append-only event log
//! today, not an indexed table) are not yet wired in.

pub mod mine;
