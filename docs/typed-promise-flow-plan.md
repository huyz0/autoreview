# Typed promise flow: implementation plan

The historical SFTPresso case in `benchmark-candidates.md` remains unscored.
Its `uploadFile` and `removeRemote` bindings are produced by a factory whose
TypeScript return annotation is a function returning `Promise<void>`. The
barrel re-exports are resolvable, but the returned function type and discarded
calls are not yet modeled. The implementation must establish those facts from
the source; matching the names of the factory or helpers is insufficient.

An exploratory parse of the pinned before revision confirms that
`createFileHandler` declares a returned callable with one required and one
optional argument, and that `removeRemote` is an immutable binding initialized
by a one-argument call to that factory. This verifies the syntax probe only; it
does not resolve the cross-file binding or score the historical case.

A later exploratory graph probe used the pinned caller, barrel, remove helper,
and factory source files and resolved `removeRemote` to the annotated factory
with one required and one optional returned-function argument. The graph was
manually restricted to that route and omitted other barrel exports, so this is
not evidence that automatic dependency discovery resolves the real project or
that the bug is detected. The case remains unscored.

A subsequent exploratory run used the normal dependency loader on complete
checkouts of both pinned revisions, with lockfile dependencies installed using
`npm ci --ignore-scripts`. It found the two discarded calls at lines 100 and
126 in the before revision and no findings in the fixed file. Both runs reached
the existing 64-dependency discovery limit, so they are incomplete and are not
scored as benchmark results. At that stage the prototype was not connected to
the normal review or replay entry points.

The normal replay subsequently passed for this pair with matching installed
dependencies available to both isolated snapshots and the configured typed
discovery budget. It matched lines 100 and 126 before the fix and no lines
after it. See `benchmarks/sftp-promise.yaml` for labels and reproduction; this
single pair is not a population-wide accuracy estimate.

On the fixed revision, a local cold-cache loader experiment found 74 reachable
files with a 256-dependency budget and reported no dependency-limit warning.
It took approximately 202 ms, versus 180 ms for the current 64-dependency
budget, which stopped at 65 files. These are single-machine measurements of
source discovery only, not full-analysis benchmarks. Normal reviews retain the
64-dependency budget; reviews with a configured typed-promise rule use a
128-dependency budget and still report exhaustion. Broader performance and
completeness checks remain necessary.

## Deliverables

1. Represent proven promise-returning callable signatures as reusable facts.
   Support explicit TypeScript `Promise<T>` return annotations, async
   declarations, and factory return annotations of the form
   `(...) => Promise<T>`. Preserve the declaration and source location that
   justify each fact. Distinguish a function returning a promise from a
   factory returning such a function.
2. Resolve those facts through immutable bindings, imports, aliases, named and
   wildcard re-exports, and supported helper returns. Respect export
   ambiguity, shadowed names, optional and rest parameters, reassignment,
   cycles, and the existing dependency and resolution budgets. Do not infer
   a fact from an unresolved module, a mutable binding, a conflicting export,
   or a locally shadowed `Promise` type.
3. Feed proven promise sources into ignored-result analysis, so a bare call,
   an unused assignment, and values passed through modeled helpers are checked
   consistently. Awaiting, returning, and otherwise observing the same value
   must remain clean. Preserve distinct call origins and changed-line related
   locations.
4. Cover promises discarded from callbacks used by `Promise.all` and related
   combinators, including callbacks with a `try`/`catch` around an unawaited
   call. Only report when the promise source is proven and the callback does
   not return or await that value on the relevant path. Exercise branches,
   nested callbacks, explicit `void`, reassignment, and synchronous
   counterexamples; do not assume every call in a map callback is asynchronous.
5. Report unsupported or budget-exhausted analysis at the affected scope and
   prevent replay from scoring that scope as clean. Keep unrelated files and
   rules usable, and preserve the existing compatibility of report consumers.

## Integration points

`Program` currently indexes lowered callable bodies by file, name, and exact
arity. Factory-produced bindings are not callable bodies, and optional
parameters make a single exact arity insufficient. Extend the shared export
resolution to return declaration identities separately from helper-frame
identities, without changing the existing helper resolution behavior. Apply
the same explicit-export precedence, ambiguity, cycle, and budget handling to
both. An exported binding that is present but cannot be typed must still
shadow wildcard exports and must not be treated as an absent export.

Carry source locations and callable arity bounds with each type fact. Resolve
the initializer's factory declaration and the resulting binding separately;
both resolutions must be unique. The current direct-import prototype trusts
its caller to supply a verified `KeyedImport`; its local `Promise` shadow
check does not establish the meaning of ambient or project-wide types. Do not
enable annotation-derived findings until that environment is resolved or an
explicit, documented contract supplies the missing guarantee.

The ignored-result engine currently gates both the entire pass and each
function on `Plan::has_registration`, and `effects::Lower::origin` obtains
sources from `Plan::source_mode`. A new source kind must participate in all
three places, as well as in helper summaries, rather than only matching calls
while lowering. Feed call-site-specific identities into those checks so local
shadowing and different calls with the same spelling cannot share a fact.
Keep the existing API-contract path unchanged and add end-to-end tests through
the normal core rule and replay entry points before claiming coverage.

An isolated TypeScript 5.9.3 probe uses the documented `getAwaitedType` checker
API and rejects `any`, unknown, generic, and mixed promise/synchronous unions.
The prototype in `crates/core/src/analyzers/typescript_promise_probe.cjs` also
declines projects with compiler errors and reports missing call locations. It
is used by an experimental built-in rule but is not a complete type-safety
proof. The Rust
prototype embeds the script, limits request and response sizes and process
time, and requires an explicitly supplied trusted compiler path. It does not
yet contain descendant processes or resolve the compiler automatically; do
not invoke an untrusted project's package as part of a review. Before
integration, decide how to supply and trust the compiler, handle subprocess
containment, report checker failures, and measure cost. Its project-wide error
gate is conservative and may skip valid calls in projects with unrelated
errors. The Node and Rust tests use `AUTOREVIEW_TEST_TYPESCRIPT` to select an
installed compiler; the Linux CI job installs a pinned version for them. A
typed contract can opt in through `AUTOREVIEW_TYPESCRIPT_MODULE`, an absolute
path to a trusted TypeScript installation. `AUTOREVIEW_TYPESCRIPT_CONFIG` can
select an absolute config path; otherwise the repository-root `tsconfig.json`
is used. `AUTOREVIEW_NODE_BINARY` optionally selects Node. Without the trusted
compiler configuration the typed contract is reported as skipped. This is an
experimental interface; compiler selection, descendant-process containment,
and normal review and replay coverage still require validation.

## Evidence and release gate

- Add positive, negative, ambiguity, shadowing, and regression fixtures for
  JavaScript, TypeScript, and TSX where the type facts are actually available.
  JavaScript must not be assumed to have TypeScript annotations; async syntax
  and explicit supported contracts can supply evidence there.
- Replay both pinned SFTPresso revisions with labels reviewed for the whole
  file. Record any remaining incomplete analysis instead of scoring it. This
  one case cannot establish project-wide precision or recall.
- Exercise helper summaries, closure and branch flow, and changed-line
  filtering alongside the new rule. Compare performance on representative
  repositories and retain bounded traversal with reported exhaustion.
- Run Autoreview and CI on the exact PR head, resolve findings, and merge only
  after the reviewed head is green. Factory-return inference and a passing
  historical replay are separate requirements; neither alone completes this
  plan.
