# Historical benchmark candidates

These candidates need a matching rule and a verified replay before they can
contribute to accuracy scores. They are not counted as detected findings.

## JavaScript promise used as a synchronous value

- Repository: https://github.com/chickenpoo351/SilphScope, MIT license at both
  pinned revisions.
- Before: `888e9e2be27eefec0bb627f124c0bfcc2b38229d`,
  `src/get-rom-config.js:10`.
- Fix: `0079a6f5d13e081dc5a26f683047895e6d1b7465`.
- Evidence: `crypto.subtle.digest` returns a promise, but the earlier code
  passes that promise to `Uint8Array` as though it were the digest buffer.
  The fix awaits the digest before constructing the array. In a local Node
  experiment with `crypto.subtle.digest('SHA-1', new Uint8Array([1, 2]))`,
  constructing a typed array from the promise produced zero elements; awaiting
  that promise first produced a 20-byte digest.

This is a promise-as-synchronous-value problem, not an ignored-result finding:
the promise is passed to a constructor, so a general result-use rule can
regard it as used. The narrowly scoped Web Crypto rule now detects this pair;
see `benchmarks/silphscope-digest.yaml` for the full-file replay. It is not a
general promise type mismatch analysis and does not track arbitrary aliases,
helpers, reassignments, or dynamically replaced globals.

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

## Ordinary TypeScript promise-returning calls

- Repository: https://github.com/actions/checkout, MIT license at both revisions.
- Before: `c952173edf28a2bd22e1a4926590c1ac39630461`.
- Fix: `5a4ac9002d0be2fb38bd78e4b4dbde5606d7042f`.
- Labels: `src/github-api-helper.ts:50` and `:73` discard the promises from
  `io.rmRF`; `src/git-auth-helper.ts:151` discards the promise from a class
  method. The fix awaits all three. The commit explains the practical race
  caused by the discarded class-method result.
- The two revisions have identical `package.json`, `package-lock.json`, and
  `tsconfig.json`. A local experiment installed the fixed revision's locked
  dependencies with `npm ci --ignore-scripts --no-audit --no-fund` and used a
  separately configured trusted TypeScript compiler. Snapshots were placed
  beneath the checkout so their imports could resolve the installed packages.
- The experimental factory-promise rule reported no findings on either file.
  This is expected from its documented factory-only scope and must not be
  counted as a false-negative score for that rule. The cases are candidates
  for a general typed promise-result rule, not completed benchmark cases.
- The factory scan selected no candidates, so that experiment does not prove
  that the compiler could type-check these snapshots. The pinned TypeScript
  3.6.4 compiler lacks the probe's `getAwaitedType` API. A separate TypeScript
  5.9.3 check with the original project configuration reports type errors,
  including errors in the old Node declarations and catch variables. A scored
  replay needs a justified, reproducible compatible compiler configuration;
  it must not silently bypass those errors.

The general `typed-promise` source has since been exercised on both revisions.
It finds all three labeled calls before the fix and none in either fixed file,
but reports unresolved call types in both revisions. This is an incomplete
observation, not a scored replay or an accuracy estimate. TypeScript 5.6.3
type-checks these snapshots with `useUnknownInCatchVariables: false`; that
restores the older compiler's catch-variable behavior without disabling
declaration checks. The opt-in regression below checks the labeled files'
SHA-256 hashes and requires the unresolved-type warnings to remain visible.

An inspection of the before revision's two labeled files found unproven calls
at `github-api-helper.ts:39,137` (`Buffer`), and `git-auth-helper.ts:54,56,148`
(`URL`, `Buffer`, and `RegExpMatchArray | null`). These are structural types;
the absence of a declared `then` property does not prove that every runtime
value lacks one. Directly awaited unknown calls and calls directly returned
from supported async functions can now be recognized as observed. With the
documented TypeScript 5.6.3 setup, replay of the before revision reported two
unresolved locations for the deletion case instead of three: the return at
`github-api-helper.ts:137` is observed by the async function. The other case
still reported 21 locations. Both cases remain incomplete and unscored; the
remaining calls and dependencies need sound result-use evidence.

The checker also recognizes an unknown result assigned to a single `const`
and immediately awaited through up to 31 consecutive immutable aliases.
It checks binding identities and retains the unresolved warning when an
intervening operation, mutable alias, conditional await, or longer chain
prevents that proof. This does not resolve the checkout replay cases above.

For a reproducible source-level observation, first follow the clone, checkout,
and dependency installation steps for this case in [the benchmark README](../benchmarks/README.md).
Then, from the `checkout` directory:

```sh
mkdir -p .autoreview-replay-tmp/checkout-before .autoreview-replay-tmp/checkout-after
git archive c952173edf28a2bd22e1a4926590c1ac39630461 | tar -x -C .autoreview-replay-tmp/checkout-before
git archive 5a4ac9002d0be2fb38bd78e4b4dbde5606d7042f | tar -x -C .autoreview-replay-tmp/checkout-after
for snapshot in checkout-before checkout-after; do
  printf '%s\n' '{"extends":"./tsconfig.json","compilerOptions":{"useUnknownInCatchVariables":false}}' > ".autoreview-replay-tmp/$snapshot/tsconfig-autoreview.json"
done
AUTOREVIEW_CHECKOUT_SNAPSHOTS="$PWD/.autoreview-replay-tmp" \
AUTOREVIEW_TEST_TYPESCRIPT="$PWD/../ts-5.6.3/node_modules/typescript" \
  cargo test --manifest-path /absolute/path/to/autoreview/Cargo.toml -p autoreview-core --lib checkout_historical_general_promise_observation
```

The normal `rules replay` entry point can also run these cases using the
benchmark pack and compatibility adapter documented in `benchmarks/README.md`.
It currently reports both cases as incomplete and exits nonzero; it does not
score either case. To score them, resolve or accurately scope the remaining
coverage limitations and check all findings in each entire file. Keep
synchronous and intentionally observed calls as negatives, and retain compiler
and snapshot verification, bounded execution, and coverage reporting.
