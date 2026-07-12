use std::path::Path;
use std::process::Command;

struct CheckResult {
    name: &'static str,
    ok: bool,
    detail: String,
}

fn check_binary(name: &'static str) -> CheckResult {
    match Command::new(name).arg("--version").output() {
        Ok(output) if output.status.success() => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let detail = stdout.lines().next().unwrap_or("found").to_string();
            CheckResult {
                name,
                ok: true,
                detail,
            }
        }
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            CheckResult {
                name,
                ok: false,
                detail: format!(
                    "exited with error: {}",
                    stderr.lines().next().unwrap_or("unknown error")
                ),
            }
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => CheckResult {
            name,
            ok: false,
            detail: "not found on PATH".to_string(),
        },
        Err(err) => CheckResult {
            name,
            ok: false,
            detail: format!("error: {err}"),
        },
    }
}

fn check_config(repo_root: &Path) -> CheckResult {
    let config_path = repo_root.join(".autoreview").join("config.yaml");
    if config_path.exists() {
        CheckResult {
            name: "config",
            ok: true,
            detail: config_path.display().to_string(),
        }
    } else {
        CheckResult {
            name: "config",
            ok: true,
            detail: "no .autoreview/config.yaml — running with defaults (run `autoreview init` to create one)".to_string(),
        }
    }
}

pub fn run_doctor(repo_root: &Path) {
    let checks = vec![
        check_binary("git"),
        check_binary("claude"),
        check_binary("ast-grep"),
        check_binary("golangci-lint"),
        check_config(repo_root),
    ];

    println!("autoreview doctor\n");
    let mut required_missing = false;
    for check in &checks {
        let icon = if check.ok { "✓" } else { "✗" };
        println!("  {icon} {:<16} {}", check.name, check.detail);
        if !check.ok && (check.name == "git" || check.name == "claude") {
            required_missing = true;
        }
    }

    println!("\nCost model assumption (transparency, not a guarantee):");
    println!(
        "  Try-cheap-first tiering only saves money if the cheap tier's pass rate exceeds the"
    );
    println!(
        "  inter-tier cost ratio — roughly 20% for a Haiku→Opus escalation at current API pricing."
    );
    println!("  If your repo's findings routinely need deep-tier escalation, quick/standard tiers won't pay for themselves.");

    if required_missing {
        println!("\nMissing required tools above — `autoreview diff` needs git and the Claude Code CLI to run.");
        std::process::exit(1);
    }
}
