use std::path::Path;

use sha2::{Digest, Sha256};

const MODULE: &str = "github.com/cilium/cilium";

pub(super) fn audited_limiter(root: &Path) -> bool {
    if root.join("go.work").exists() || root.join("pkg/rate/go.mod").exists() {
        return false;
    }
    let Ok(module) = std::fs::read_to_string(root.join("go.mod")) else {
        return false;
    };
    if !module
        .lines()
        .next()
        .is_some_and(|line| line.split_whitespace().eq(["module", MODULE]))
    {
        return false;
    }
    // In the audited source, Wait acquires one token. With burst one the
    // explicit burst error is unreachable, and Acquire returns only ctx.Err().
    [
        (
            "pkg/rate/limiter.go",
            "293e1fc2506c53dfb7de0cc9bb1266ccc75f8d2c4c04932b97d123291c5ebbcd",
        ),
        (
            "vendor/golang.org/x/sync/semaphore/semaphore.go",
            "b75801ca432d216a980f41d56e8ded5523950f3a0421c165a9237629800e1596",
        ),
    ]
    .iter()
    .all(|(path, hash)| {
        std::fs::read_to_string(root.join(path)).is_ok_and(|source| {
            format!("{:x}", Sha256::digest(source.replace("\r\n", "\n"))) == *hash
        })
    })
}
