# Experimental Go error type evidence

This experimental probe uses `go/types` to check whether specified calls
return one or more predeclared `error` results. Autoreview can invoke package mode when
explicitly configured with trusted probe and Go executable paths; the probe
itself does not decide whether a result is discarded. It has two input modes:
caller-supplied source files and experimental package loading. Both use JSON on
standard input, limited to 1 MiB.

In source-file mode the caller supplies every build-selected source file:

```json
{"files":[{"path":"main.go","source":"package p\nfunc f() error { return nil }\nfunc g() { f() }\n"}],"calls":[{"path":"main.go","start":51}]}
```

In package mode, provide an absolute package directory, an absolute trusted Go
binary, and SHA-256 hashes of every source file containing a requested call.
Call paths are package-relative file names:

```json
{"packageDir":"/project/pkg","goBinary":"/trusted/go/bin/go","expectedSources":{"main.go":"<sha256-of-analyzed-bytes>"},"calls":[{"path":"main.go","start":51}]}
```

Package mode runs `go list -deps -export -json .` with module downloads and
toolchain downloads disabled and `GOFLAGS=-mod=readonly`. It uses the Go environment's
platform, workspace, and cache, applies default build tags, loads dependency
types, and reads the
selected ordinary Go files. Cached dependencies are required. It rejects a
package with cgo files and does not load test variants. Missing call-file hashes or mismatches for any supplied source hash fail the
whole request. A Go command and its build tools can
execute processes and read project configuration; use only trusted tools and
projects in an appropriately contained environment. The 60-second direct
child timeout does not yet contain descendant processes.

To use an existing consistent vendor tree, set `moduleMode` to `vendor` in a
package-mode request, or set `AUTOREVIEW_GO_MODULE_MODE=vendor` when invoking
the probe through Autoreview. The Go command then uses `-mod=vendor`; network
downloads remain disabled. Other values are rejected, except `readonly` and
the empty default. An older probe that cannot accept the mode reports
incomplete analysis rather than silently changing dependency sources. Vendor
files and dependency exports are not yet verified as an immutable snapshot.

Each `start` is a zero-based UTF-8 byte offset of the call expression, not a
line or character number. Responses retain request order and report `error`
for a single predeclared `error` result, or `multi_error` with the result count
and zero-based error indexes for multi-result calls. `not_proven` means a
resolved call does not have such a result; `unavailable` means the requested
call was not found. Syntax, type-checking,
package-load, or source-identity failures produce an `incomplete` response.
`error` conversions are not classified as calls returning errors. Unsupported
multi-result contexts are reported as incomplete by Autoreview.

Requests may set `includeCallees` to `true` to include the declaring package,
function name, and (for methods) receiver type name for supported statically
bound calls, including explicit generic instantiations. Missing `callee` means
the target was not established; interface dispatch and function-valued
variables are deliberately unresolved. This identifies a declaration,
not the errors it can return or a guarantee about unverified dependencies.
In package mode, a request for callee evidence must supply matching hashes for
every build-selected source file in the target package. The optional
`packageSources` map can include additional unselected `.go` files; the
separate `expectedSources` map still requires every analyzed source to be
build-selected and match. It still does not
verify the contents of dependency exports or prevent subsequent changes.
With `AUTOREVIEW_GO_CALLEE_EVIDENCE=1`, Autoreview requests this evidence for
the experimental Go result-use rule and attaches available identities to
finding metadata as `goTypeCheckedCallee`. It matches the original call's byte
offset and omits ambiguous merged findings. If the probe is older or the
package hashes are incomplete, Autoreview retries without callee evidence;
the existing result-use analysis remains available. This evidence does not
suppress findings or establish a callee's error behavior.
Autoreview hashes at most 512 Go files totaling 8 MiB in a package directory;
unreadable files, symlinks, or exceeded limits cause a fallback.

The probe also accepts an experimental `includeErrorOrigins` request flag.
For a statically resolved function whose body is in the loaded package, it may
report `context_error_or_nil` and the zero-based context parameter index,
excluding any method receiver. This
means the supported explicit return expressions are the parameter's `Err()`,
predeclared `nil`, or a direct call to a same-package helper with an already
established summary and the same context parameter. Helper summaries use at
most 16 passes; unresolved or recursive-only flows stay unknown. It does not
prove that the context implementation is
well behaved, that callers check the error, or that suppressing it is correct.
Named returns, deferred or nested functions, reassigned or escaping context
parameters, other returned expressions, and unavailable source produce no
summary. Package-mode requests require complete selected-file hashes as for
callee evidence. Set `AUTOREVIEW_GO_ERROR_ORIGIN_EVIDENCE=1` to attach available
summaries as `goErrorOrigin` metadata on experimental result-use findings.
This does not change finding decisions. Unsupported or older probes fall back
to the existing analysis without that metadata.

The caller must analyze an immutable package snapshot: other package files and
cached dependency exports can change during or after loading. Source-file mode
still relies on the caller to provide the complete build-selected package.
Explicit build tags, test variants, cgo, full dependency and source snapshot
identity, and process-tree containment remain unsupported. Autoreview bounds
requests and responses and reports unavailable evidence as a coverage limit;
it does not treat that evidence as a clean result.

With the pinned yq revision and its dependencies installed in an isolated
cache, package mode classified the three candidate calls as returning `error`.
The integrated experimental rule's historical replay also finds the three
labeled discarded results and no findings after the fix. This single fix pair
is not an estimate of general precision or recall.

Run the tests with a Go toolchain:

```sh
cd tools/go-error-evidence
GOPROXY=off GOSUMDB=off go test ./...
```

The response protocol is versioned. Rebuild the probe after updating Autoreview;
an older probe is rejected as incomplete rather than silently missing results.
