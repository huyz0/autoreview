//! Bounded interface to the optional TypeScript type-evidence prototype.
//! The compiler path is supplied explicitly by the caller and executes code;
//! it must point to a trusted TypeScript installation.

use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};
use serde::{Deserialize, Serialize};

const SCRIPT: &str = include_str!("typescript_promise_probe.cjs");
const MAX_BYTES: usize = 256 * 1024;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TypeScriptRequest<'a> {
    pub typescript_module: &'a str,
    pub config: &'a str,
    pub calls: &'a [TypeScriptCall],
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct TypeScriptCall {
    pub path: String,
    pub start: usize,
}

#[derive(Debug, Deserialize)]
pub struct TypeScriptResult {
    pub status: String,
    pub reason: Option<String>,
    pub calls: Option<Vec<TypeScriptCallResult>>,
}

#[derive(Debug, Deserialize)]
pub struct TypeScriptCallResult {
    pub path: String,
    pub start: usize,
    pub status: String,
}

/// Runs an explicitly supplied compiler with bounded input, output and time.
/// Any failure returns `None` and must be reported as incomplete analysis.
pub fn check(node: &str, request: &TypeScriptRequest<'_>, timeout: Duration) -> Option<TypeScriptResult> {
    let input = serde_json::to_vec(request).ok()?;
    if input.len() > MAX_BYTES { return None; }
    let mut child = Command::new(node).args(["-e", SCRIPT]).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().ok()?;
    let mut stdin = child.stdin.take()?;
    let mut stdout = child.stdout.take()?;
    let (written_tx, written_rx) = mpsc::sync_channel(1);
    let (output_tx, output_rx) = mpsc::sync_channel(1);
    std::thread::spawn(move || { let _ = written_tx.send(stdin.write_all(&input)); });
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = stdout.by_ref().take((MAX_BYTES + 1) as u64).read_to_end(&mut bytes).map(|_| bytes);
        let _ = output_tx.send(result);
    });
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if started.elapsed() < timeout => std::thread::sleep(Duration::from_millis(10)),
            _ => { let _ = child.kill(); let _ = child.wait(); break None; }
        }
    };
    let written = written_rx.recv_timeout(timeout.saturating_sub(started.elapsed())).ok()?.is_ok();
    let output = output_rx.recv_timeout(timeout.saturating_sub(started.elapsed())).ok()?.ok()?;
    if !written || !status?.success() || output.len() > MAX_BYTES { return None; }
    let result: TypeScriptResult = serde_json::from_slice(&output).ok()?;
    match result.status.as_str() {
        "incomplete" if result.reason.is_some() && result.calls.is_none() => Some(result),
        "ok" if result.reason.is_none() && result.calls.as_ref().is_some_and(|calls| {
            calls.len() == request.calls.len() && calls.iter().zip(request.calls).all(|(actual, expected)| {
                actual.path == expected.path && actual.start == expected.start
                    && matches!(actual.status.as_str(), "promise_like" | "not_proven" | "unavailable")
            })
        }) => Some(result),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checker_proves_a_call_and_fails_closed() {
        let Ok(compiler) = std::env::var("AUTOREVIEW_TEST_TYPESCRIPT") else { return };
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("tsconfig.json");
        let file = dir.path().join("sample.ts");
        let source = "declare function f(): Promise<void>;\nf();\n";
        std::fs::write(&config, r#"{"compilerOptions":{"target":"ES2022","strict":true},"files":["sample.ts"]}"#).unwrap();
        std::fs::write(&file, source).unwrap();
        let calls = [TypeScriptCall { path: file.to_string_lossy().into_owned(), start: source.rfind("f()").unwrap() }];
        let config = config.to_string_lossy();
        let request = TypeScriptRequest { typescript_module: &compiler, config: &config, calls: &calls };
        let result = check("node", &request, Duration::from_secs(20)).unwrap();
        assert_eq!(result.status, "ok");
        assert_eq!(result.calls.unwrap()[0].status, "promise_like");
        assert!(check("node", &request, Duration::ZERO).is_none());
        let absent = TypeScriptRequest { typescript_module: "missing-autoreview-typescript", ..request };
        assert_eq!(check("node", &absent, Duration::from_secs(20)).unwrap().reason.as_deref(), Some("checker_unavailable"));
        let noisy = dir.path().join("noisy.cjs");
        std::fs::write(&noisy, "process.stdout.write('x'.repeat(300000)); for (;;) {};").unwrap();
        let noisy_path = noisy.to_string_lossy();
        let noisy_request = TypeScriptRequest { typescript_module: &noisy_path, ..absent };
        let before = Instant::now();
        assert!(check("node", &noisy_request, Duration::from_millis(200)).is_none());
        assert!(before.elapsed() < Duration::from_secs(5));
    }
}
