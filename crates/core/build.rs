//! `ast_grep.rs` embeds `rules-builtin/` via `include_dir!`, which tracks
//! already-known files' *content* for incremental rebuilds but not the
//! directory's own membership — adding or removing a rule file with no
//! `.rs` change anywhere silently leaves a stale binary embedding the old
//! rule set. Declaring the directory here makes cargo watch it directly.

fn main() {
    println!("cargo:rerun-if-changed=rules-builtin");
}
