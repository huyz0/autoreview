# Callback execution evidence

## Problem

The Kotlin coroutine patterns use lexical nesting to associate an operation
with a suspend function. That is not enough for a lambda: `map { ... }` can
execute its body at the call site, while a functional-interface conversion can
construct a callback for later use. The RediCloud replay in
`benchmarks/redicloud-coroutines.yaml` captures this distinction. Its
`PasswordAuthenticator` lambda is registered with an SSH server; the same
callback is reported in both revisions even though the fix removes the direct
blocking calls. The replay remains intentionally unsuccessful until the rule
can distinguish these cases.

Excluding every lambda would hide the existing `map` positive fixture. Treating
every constructor-like name as a functional interface would also be unsound:
names can be shadowed, and a regular function can invoke its argument while
constructing a value. An imported type is useful evidence, but the current
pattern engine does not resolve its Java declaration or its receiver type.

## Intended behavior

Introduce a shared, bounded execution-context analysis for rules that need it.
For each operation within a callback, classify its relationship to the
enclosing call as **immediate**, **deferred**, or **unknown**. Evidence must
identify the resolved callee, the callback argument, and the reason for the
classification. A rule may use that result to decide whether the operation
belongs to its enclosing execution context; unknown must never silently become
either a definite finding or a clean negative.

Initially target Kotlin coroutine checks, but make contracts independent of
the individual rule and language. The model must distinguish a functional
interface value from its later invocation, including immediate invocation of
a newly constructed value and local aliases. It must not assume a library
method is inline, deferred, or harmless solely because its name matches a
known method. Contracts need a resolved declaration identity and argument
position; local declarations, aliases, imports, overloads, type aliases and
shadowing must be tested before an identity is trusted.

## Delivery sequence

The Kotlin pattern coverage check recognizes directly invoked lambda literals
such as `({ ... })()`. It still reports an unresolved outer lambda, even when
an inner lambda is directly invoked. Alias calls, `.invoke()` forms, inline
library methods, and functional-interface conversions remain unresolved.

1. Add an execution-context result to the shared analysis model, including
   source and callback locations, evidence identity, and an explicit unknown
   reason. Define path and analysis budgets and propagate exhaustion as
   coverage, scoped to the affected rule and function.
2. Resolve a bounded Kotlin subset: named functions and anonymous functions,
   directly invoked lambdas, supported local aliases, and uniquely resolved
   functional-interface conversions. Treat dynamic dispatch, ambiguous imports,
   unresolved classpaths, escaping values, and unsupported syntax as unknown.
   Do not infer that a stored callback is never invoked before the function
   returns without proving the relevant call paths.
3. Add versioned, declarative contracts for known APIs, with language, resolved
   symbol identity, callback parameter, execution timing, and provenance.
   Validate contracts at load time and use the same definitions for bundled and
   external packs. Keep framework-specific assumptions out of generic syntax
   matching. Begin with a verified Apache SSHD registration contract and a
   verified Kotlin inline operation; expand only with evidence and negatives.
4. Connect the Kotlin coroutine rules to execution evidence through a supported
   rule-engine feature, preserving stable rule IDs. A raw ast-grep invocation
   cannot evaluate the extra semantic condition: document that limitation and
   make Autoreview's behavior and exported rule metadata explicit rather than
   silently changing one execution path. Review, replay, and gating must share
   the same condition and incomplete-analysis behavior.
5. Extend only after parity tests establish equivalent semantics for other
   supported languages. Java and Go callbacks, JavaScript/TypeScript/TSX
   handlers and promises, and Rust closures differ in dispatch, ownership, and
   scheduling; each needs its own resolution and execution evidence.

## Required verification

- Exercise direct calls, inline `map`, a stored callback, an immediately
  invoked callback, nested callbacks, and callbacks invoked through a local
  alias. Include early returns, branches, reassignments, and callbacks whose
  execution is conditional.
- Exercise shadowed names, unrelated same-name APIs, renamed and wildcard
  imports, ambiguous dependencies, conflicting declarations, overloaded calls,
  unknown receivers, unsupported expressions, and exhausted budgets. None may
  be treated as verified solely because a name resembles a known API.
- Replay the RediCloud before and after snapshots. The two direct calls should
  remain findings; the registered authenticator should not be attributed to
  the enclosing suspend function. The case must complete with no mislabeled
  findings or hidden coverage limitations. Preserve a separate fixture for an
  immediately invoked authenticator so the same callback body is not always
  suppressed.
- Verify the other three Kotlin coroutine rules as well as the blocking rule;
  exercise every built-in positive and negative fixture and run the full rule
  conformance suite. Add CLI review and replay tests for both completed and
  incomplete cases, including changed lines and an unknown callback identity.
- Run historical replays, Autoreview on the final diff, and hosted CI. Record
  measured results and remaining limitations; one passing fix pair cannot
  establish general precision or recall.

## Open design decisions

The repository currently has no Kotlin classpath-backed resolver. Before
claiming a third-party interface is resolved, choose and test a trusted source
of declaration identity, such as an optional compiler-backed probe or a
verified, versioned dependency index. A syntactic import alone is insufficient
when local or dependency declarations can shadow it. Establish how such a
probe behaves on unavailable dependencies and snapshots, including whether it
executes project tooling, before making it a default.

The existing ast-grep pipeline does not carry structured coverage for an
individual uncertain match. Adding this condition therefore requires either a
new semantic rule backend or an explicit post-processing contract with shared
review, replay, and gate behavior. Do not implement a CLI-only filter that
silently drops findings or makes replay score an unresolved callback as clean.
