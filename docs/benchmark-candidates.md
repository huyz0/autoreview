# Historical benchmark candidates

These candidates need a matching rule and a verified replay before they can
contribute to accuracy scores. They are not counted as detected findings.

## Project-defined Go error returns

- Repository: https://github.com/taigrr/yq, MIT license at the pinned revisions.
- Before: `f8a700e4005dbf47528eaee3060e90ae5d41b5e4`,
  `pkg/yqlib/operator_explode.go:21`, `:68`, and `:72`.
- Fix: `af39fc737dc49317dafe3adf99fb24749f759482`.
- Evidence: the earlier code calls `explodeNode` without using its error;
  the fix propagates that error. Two further calls discard `applyAlias` errors;
  the same fix propagates those too. Both functions are declared in the file.

The experimental typed Go rule now detects all three calls in the historical
replay and none in the corrected file; see `benchmarks/yq-error.yaml`. This one
fix pair does not establish general precision or recall, and the checker remains
opt-in with documented package and runtime limitations.

## Promise-returning TypeScript helpers in `Promise.all`

- Repository: https://github.com/jmwerk/SFTPresso, MIT license at the pinned revisions.
- Before: `be43f28401e0ba693e9bfe0a1b27476d9eb4117a`,
  `src/commands/commandUploadChangedFiles.ts:100` and `:126`.
- Fix: `460fa47433bf6ed423cc84cb0939ce6b90246e6a`.
- Evidence: both callbacks call a promise-returning imported helper without
  returning or awaiting the promise. `Promise.all` therefore receives
  callbacks that return `undefined`, and their surrounding `catch` blocks
  cannot catch later rejections. The fix awaits each helper in an async callback.
  The helpers are exposed through a barrel export and created by a factory
  whose return type is `Promise<void>`.

The typed factory rule now detects this case with an explicitly configured,
trusted TypeScript compiler. The normal replay and its limits are documented in
`benchmarks/sftp-promise.yaml` and `benchmarks/README.md`. One fix pair cannot
establish general precision or recall.
