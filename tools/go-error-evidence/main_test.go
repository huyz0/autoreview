package main

import (
	"strings"
	"testing"
)

func TestAnalyzeCallResults(t *testing.T) {
	src := `package p
func operation() error { return nil }
func other() (int, error) { return 0, nil }
func good() int { return 0 }
type custom interface { Error() string }
func customResult() custom { return nil }
type alias = error
func aliased() alias { return nil }
func run() {
  operation()
  other()
  good()
  customResult()
  aliased()
  _ = error(nil)
}
`
	snippets := []string{"operation()\n", "other()\n", "good()\n", "customResult()\n", "aliased()\n", "error(nil)"}
	want := []string{"error", "multi_error", "not_proven", "not_proven", "error", "not_proven"}
	req := request{Files: []sourceFile{{Path: "main.go", Source: src}}}
	for _, snippet := range snippets {
		pos := strings.LastIndex(src, snippet)
		if pos < 0 {
			t.Fatalf("missing %q", snippet)
		}
		req.Calls = append(req.Calls, callSite{Path: "main.go", Start: pos})
	}
	got := analyze(req)
	if got.Status != "ok" {
		t.Fatalf("unexpected result: %+v", got)
	}
	if got.Calls[1].ResultCount != 2 || len(got.Calls[1].ErrorIndexes) != 1 || got.Calls[1].ErrorIndexes[0] != 1 {
		t.Fatalf("multi-result positions: %+v", got.Calls[1])
	}
	for i, expected := range want {
		if got.Calls[i].Status != expected {
			t.Errorf("%s: got %s, want %s", snippets[i], got.Calls[i].Status, expected)
		}
	}
}

func TestAnalyzeUsesAllPackageFiles(t *testing.T) {
	caller := "package p\nfunc run() { operation() }\n"
	provider := "package p\nfunc operation() error { return nil }\n"
	req := request{Files: []sourceFile{{"caller.go", caller}, {"provider.go", provider}}, Calls: []callSite{{"caller.go", strings.Index(caller, "operation()")}}}
	if got := analyze(req); got.Status != "ok" || got.Calls[0].Status != "error" {
		t.Fatalf("unexpected: %+v", got)
	}
	req.Files = req.Files[:1]
	if got := analyze(req); got.Status != "incomplete" || got.Reason != "type_errors" {
		t.Fatalf("missing file should be incomplete: %+v", got)
	}
}

func TestAnalyzeRejectsIncompleteAndShadowedEvidence(t *testing.T) {
	src := "package p\ntype error int\nfunc operation() error { return 1 }\nfunc run() { operation() }\n"
	req := request{Files: []sourceFile{{"main.go", src}}, Calls: []callSite{{"main.go", strings.LastIndex(src, "operation()")}}}
	if got := analyze(req); got.Status != "ok" || got.Calls[0].Status != "not_proven" {
		t.Fatalf("shadowed error: %+v", got)
	}
	req.Files = append(req.Files, req.Files[0])
	if got := analyze(req); got.Status != "incomplete" || got.Reason != "invalid_files" {
		t.Fatalf("duplicate file: %+v", got)
	}
	req.Files = []sourceFile{{"bad.go", "package p\nfunc broken("}}
	if got := analyze(req); got.Status != "incomplete" || got.Reason != "syntax_errors" {
		t.Fatalf("syntax error: %+v", got)
	}
}

func TestAnalyzeMissingCallIsUnavailable(t *testing.T) {
	src := "package p\nfunc run() {}\n"
	req := request{Files: []sourceFile{{"main.go", src}}, Calls: []callSite{{"main.go", 0}, {"missing.go", 0}}}
	got := analyze(req)
	if got.Status != "ok" || len(got.Calls) != 2 || got.Calls[0].Status != "unavailable" || got.Calls[1].Status != "unavailable" {
		t.Fatalf("missing call must not be treated as disproven: %+v", got)
	}
}
func TestMethodsGenericsAndUTF8Offsets(t *testing.T) {
	src := `package p
// résumé
 type item struct{}
func (item) run() error { return nil }
func generic[T any](value T) error { return nil }
func use() {
 item{}.run()
 generic(42)
}
`
	req := request{Files: []sourceFile{{"main.go", src}}, Calls: []callSite{
		{"main.go", strings.Index(src, "item{}.run()")},
		{"main.go", strings.Index(src, "generic(42)")},
	}}
	got := analyze(req)
	if got.Status != "ok" || len(got.Calls) != 2 || got.Calls[0].Status != "error" || got.Calls[1].Status != "error" {
		t.Fatalf("method, generic, and UTF-8 call offsets: %+v", got)
	}
}
