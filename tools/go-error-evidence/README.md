# Experimental Go error type evidence

This standalone prototype checks caller-supplied Go sources with `go/types`.
It does not discover packages, apply build tags, run Autoreview, or report
findings. It is not a replacement for a full package loader.

Input is one JSON object on standard input, limited to 1 MiB:

```json
{"files":[{"path":"main.go","source":"package p\nfunc f() error { return nil }\nfunc g() { f() }\n"}],"calls":[{"path":"main.go","start":51}]}
```

Each `start` is a zero-based UTF-8 byte offset of the call expression, not a
line or character number. For a type-correct package, responses retain request
order and report `error` only when a callable has exactly one result identical
to Go's predeclared `error`; `not_proven` means a resolved call does not meet
that condition, while `unavailable` means the requested call was not found.
Invalid source, duplicate paths, or any type-checking error produce an
`incomplete` response. Multi-result calls are intentionally not classified as
single-error calls. An `error` conversion is not a callable result.

The caller must supply every source file selected for the package by the Go
build, including effects of platform, build tags, and test variants. Omitting
files can change type resolution without causing an error. `go/types` uses the
standard compiled-package importer; unavailable imports lead to an incomplete
result. This prototype does not load modules or download dependencies. A
future integration must establish complete package selection and dependency
resolution, bound runtime and process trees, validate source identity and
response size, and surface failures as coverage limitations. It must not trust
a partial package as proof or silently treat unavailable evidence as clean.

Run the tests with a Go toolchain:

```sh
cd tools/go-error-evidence
GOPROXY=off GOSUMDB=off go test ./...
```
