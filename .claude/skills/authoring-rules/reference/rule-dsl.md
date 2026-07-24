# autoreview rule DSL reference

All examples below are real rules from this repo's own `crates/core/rules-builtin/`
(or the equivalent path in whatever repo you're working in, if autoreview
rules are vendored there) — copy the shape, not just the idea.

## Contents

- [Fields common to every rule](#fields-common-to-every-rule)
- [Category taxonomy](#category-taxonomy)
- [`kind: pattern` (default)](#kind-pattern-default)
- [`kind: taint`](#kind-taint)
- [`kind: threshold`](#kind-threshold)
- [`kind: call-sequence`](#kind-call-sequence)
- [`semantic: true`](#semantic-true)
- [Rule pack layout](#rule-pack-layout)
- [Validation requirements](#validation-requirements)

## Fields common to every rule

```yaml
id: go-example-rule-id       # unique within the pack; kebab-case, usually <lang>-<what>
language: Go                 # Go | Java | Kotlin | JavaScript | TypeScript
category: correctness        # see taxonomy below
severity: warning            # error | warning | info
kind: pattern                 # pattern (default, omit if pattern) | taint | threshold | call-sequence
semantic: false                # optional, default false — see semantic: true below
message: "..."                # shown to the reviewer; see per-kind notes on {placeholder} support
metadata:                     # optional
  cwe: ["CWE-78"]
  owasp: ["A03:2021"]
  confidence: HIGH            # HIGH | MEDIUM | LOW
  references:
    - "https://cwe.mitre.org/data/definitions/78.html"
```

`id` must be unique within its pack — `autoreview rules packs validate`
rejects a duplicate. `language`/`category`/`severity`/`message` are always
required; `kind` defaults to `pattern` if omitted.

## Category taxonomy

Four categories are actually in use across this codebase's ~200 builtin
rules — pick the closest fit, don't invent a fifth:

- **`security`** — an exploitable weakness (injection, XXE, SSRF, weak
  crypto, prototype pollution, ...).
- **`correctness`** — a real bug or a path that produces wrong behavior
  (unreleased lock, nil dereference, self-comparison, ...).
- **`design`** — a structural smell (god class, utility class with a public
  constructor, layer-import violation, ...) — not wrong, but worth a second
  look.
- **`performance`** — a real cost (object allocation in a loop, nested
  linear search, N+1-shaped access, ...).

## `kind: pattern` (default)

Native ast-grep syntax under `rule:`, handed to the `ast-grep` subprocess
unchanged — this is the only kind where `rule:`/`constraints:` mean exactly
what they'd mean in a raw ast-grep config. Use for a syntactic match with no
dataflow or call-ordering question attached.

```yaml
id: javascript-command-injection-concat
language: JavaScript
category: security
severity: error
message: A shell command built via string concatenation and passed to
  `child_process.exec`/`execSync` is vulnerable to command injection —
  `exec` always runs its argument through a shell. Use `execFile`/`spawn`
  with a separate argument array instead, which never invokes a shell.
metadata:
  cwe: ["CWE-78"]
  owasp: ["A03:2021"]
  confidence: HIGH
rule:
  any:
    - pattern: exec($CMD)
    - pattern: execSync($CMD)
    - pattern: $CP.exec($CMD)
    - pattern: $CP.execSync($CMD)
constraints:
  CMD:
    regex: '".*"\s*\+'
```

`$UPPER_CASE` names are ast-grep metavariables (match anything, bind it for
reuse); `$$$NAME` matches a variadic list of nodes. `any:`/`all:`/`not:`
compose sub-rules. `constraints:` narrows what a metavariable is allowed to
match (here: `CMD` must look like string concatenation).

## `kind: taint`

Source → sink dataflow: does a value that originates at a `sources` call
reach a `sinks` call without passing through a `sanitizers` call first? This
needs real interprocedural tracing (`crates/dataflow`), not something a bare
`pattern` rule can express.

```yaml
id: go-command-injection-taint
language: Go
category: security
severity: error
kind: taint
semantic: true
message: "`{tainted_arg}` reaches `{sink_call}` with an unsanitized value from an HTTP form field"
sources:
  - call: FormValue
  - call: PostFormValue
sinks:
  - call: exec.Command
  - call: exec.CommandContext
  - call: syscall.Exec
  - call: syscall.ForkExec
  - call: exec.Cmd{Path}
sanitizers: []
```

`message` supports `{tainted_arg}`/`{sink_call}` placeholders, filled in
per finding. `sanitizers` (often empty, as above — the engine still tracks
data through unrelated calls even with none) lists calls that neutralize the
taint if the tainted value passes through them first, e.g.
`- call: filepath.Clean` for a path-traversal rule.

## `kind: threshold`

A single numeric metric compared against a fixed bound — no `rule:` block
at all, since there's no pattern to match, just a computed number per
function/class/file.

```yaml
id: utility-class-public-constructor
language: Java
category: design
severity: warning
kind: threshold
metric: utility-class-min-static-methods
threshold: 2
message: "Utility class with a public constructor"
```

Metrics already wired up (pick one of these — a new metric name needs a new
implementation in `complexity.rs`, not just a YAML file, so don't invent
one): `cyclomatic-complexity`, `cognitive-complexity`, `deep-nesting`,
`long-method`, `long-parameter-list`, `too-many-returns`, `large-switch`,
`god-class`, `complex-interface`, `data-class-min-accessors`,
`utility-class-min-static-methods`.

## `kind: call-sequence`

A boolean "pending" fact along a CFG path: after calling `after`, was
`unless` called before either `before` or (if `checkBeforeReturn: true`) a
`return`? Use for must-follow/must-precede conventions across statements —
lock/unlock, open/close, a required check before use.

```yaml
id: java-unreleased-lock
language: Java
category: correctness
severity: error
kind: call-sequence
message: "A `Lock` was acquired but this path returns without releasing it — ..."
after:
  - call: lock
unless:
  - call: unlock
checkBeforeReturn: true
```

Another real shape — `before:` instead of (or alongside) `checkBeforeReturn`,
for "must happen before this specific other call", not just "before
returning":

```yaml
after:
  - call: newInstance
unless:
  - call: setFeature
  - call: setExpandEntityReferences
before:
  - call: parse
```

## `semantic: true`

Independent of `kind` — marks a rule as syntactically precise but
semantically approximate (no real type resolution or dataflow backing the
match), so every finding it produces automatically gets a Stage 3.5
cheap-LLM confirmation pass before being trusted, regardless of category or
severity. Set it whenever the match could plausibly be a false positive a
human would catch in half a second (e.g. a nested-loop-looks-like-linear-search
heuristic) — not required for a `pattern` rule that's exact by construction
(a literal syntactic anti-pattern like `$X == $X`), but common on `taint`/
`call-sequence` rules since those are inherently approximate without full
type information.

## Rule pack layout

A rule pack is a directory with a manifest plus one YAML file per rule,
organized by convention (not enforced) as `<language>/<category>/<id>.yml`:

```
my-rules/
├── rulepack.yaml
└── go/
    └── security/
        └── go-hardcoded-secret.yml
```

`rulepack.yaml`:

```yaml
id: acme-security
version: "1.0.0"
description: Acme's internal security conventions
```

## Validation requirements

`autoreview rules packs validate <path>` checks, per rule file, exactly
this (nothing more, nothing you need to guess):

- `id` is present and non-empty, unique within the pack.
- `kind: pattern` (or omitted): a `rule:` block is present.
- `kind: taint`: a `sinks:` list is present (non-empty).
- `kind: threshold`: `metric:` and `threshold:` are both present.
- `kind: call-sequence`: `after:` and `unless:` are both present and
  non-empty (`before:`/`checkBeforeReturn` aren't checked up front — at
  least one of them still needs to be set for the rule to ever actually
  fire, but that's on you to get right, same as picking a real `metric:`
  name for `threshold`).
