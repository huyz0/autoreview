//! Explicit, honest stub for the one part of the CLI surface that depends on
//! infrastructure not built yet: the rule factory behind `rules` (M3).
//! `apply` and `feedback` both graduated out of this file as their
//! infrastructure landed — see `commands::apply` and `commands::feedback`.
//! This exists as a real subcommand — with real `--help` text — rather than
//! being silently absent, so the command surface documents its own roadmap
//! instead of just erroring "unrecognized subcommand".

pub fn run_rules_stub(action: &str) {
    println!("`autoreview rules {action}` is not implemented yet — planned for M3 (rule factory: mine -> draft -> bench -> candidate -> shadow -> promote), per the project plan.");
}
