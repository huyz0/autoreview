use std::path::Path;
use autoreview_dataflow::call_order::KeyedLimitation;

pub(super) fn package_limitation(root: &Path, dir: &Path, reason: &'static str) -> KeyedLimitation {
    let path = if is_package_reason(reason) {
        root.canonicalize().ok().and_then(|root| dir.strip_prefix(root).ok().map(Path::to_owned))
            .and_then(|path| path.to_str().map(|value| if value.is_empty() { ".".into() } else { value.replace('\\', "/") }))
    } else { None };
    KeyedLimitation { reason, path, line: None }
}

pub(super) fn is_package_reason(reason: &str) -> bool {
    matches!(reason, "go_error_package_limit" | "go_error_call_limit" | "go_error_package_load_incomplete" |
        "go_error_typecheck_incomplete" | "go_error_source_mismatch" | "go_error_probe_invalid_request")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_limitations_only_expose_paths_within_the_repository() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let package = root.path().join("pkg").join("worker");
        std::fs::create_dir_all(&package).unwrap();
        let root = root.path().canonicalize().unwrap();
        let package = package.canonicalize().unwrap();
        let outside = outside.path().canonicalize().unwrap();
        assert_eq!(package_limitation(&root, &package, "go_error_call_limit").path.as_deref(), Some("pkg/worker"));
        assert_eq!(package_limitation(&root, &root, "go_error_call_limit").path.as_deref(), Some("."));
        assert!(package_limitation(&root, &outside, "go_error_call_limit").path.is_none());
        assert!(package_limitation(&root, &package, "go_error_checker_incomplete").path.is_none());
    }
}
