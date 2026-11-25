package main

import (
	"crypto/sha256"
	"encoding/hex"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
)

func TestPackageModeUsesBuildFilesAndLocalDependencies(t *testing.T) {
	t.Setenv("GOFLAGS", "-tags=never")
	binary, err := exec.LookPath("go")
	if err != nil {
		t.Skip("Go toolchain unavailable")
	}
	binary, err = filepath.Abs(binary)
	if err != nil {
		t.Fatal(err)
	}
	dir := t.TempDir()
	write := func(path, data string) {
		t.Helper()
		full := filepath.Join(dir, path)
		if err := os.MkdirAll(filepath.Dir(full), 0755); err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(full, []byte(data), 0644); err != nil {
			t.Fatal(err)
		}
	}
	write("go.mod", "module example.test/probe\n\ngo 1.22\n")
	write("dep/dep.go", "package dep\nfunc Operation() error { return nil }\n")
	src := "package sample\nimport \"example.test/probe/dep\"\nfunc run() { dep.Operation() }\n"
	write("main.go", src)
	write("ignored.go", "//go:build never\n\npackage sample\nthis is not Go\n")
	sum := sha256.Sum256([]byte(src))
	req := request{PackageDir: dir, GoBinary: binary, ExpectedSources: map[string]string{"main.go": hex.EncodeToString(sum[:])}, Calls: []callSite{{"main.go", strings.Index(src, "dep.Operation()")}}}
	got := analyze(req)
	if got.Status != "ok" || len(got.Calls) != 1 || got.Calls[0].Status != "error" {
		t.Fatalf("package evidence: %+v", got)
	}
	req.ExpectedSources["main.go"] = strings.Repeat("0", 64)
	if got := analyze(req); got.Status != "incomplete" || got.Reason != "source_mismatch" {
		t.Fatalf("changed source: %+v", got)
	}
	req.ExpectedSources = nil
	if got := analyze(req); got.Status != "incomplete" || got.Reason != "source_mismatch" {
		t.Fatalf("missing digest: %+v", got)
	}
}

func TestPackageModeRejectsInvalidConfiguration(t *testing.T) {
	got := analyze(request{PackageDir: "relative", GoBinary: "go"})
	if got.Status != "incomplete" || got.Reason != "invalid_package_config" {
		t.Fatalf("relative configuration: %+v", got)
	}
	got = analyze(request{PackageDir: "relative", Files: []sourceFile{{"main.go", "package p"}}})
	if got.Status != "incomplete" || got.Reason != "invalid_request" {
		t.Fatalf("mixed modes: %+v", got)
	}
}
func TestPackageModeReportsMissingDependency(t *testing.T) {
	binary, err := exec.LookPath("go")
	if err != nil {
		t.Skip("Go toolchain unavailable")
	}
	binary, err = filepath.Abs(binary)
	if err != nil {
		t.Fatal(err)
	}
	dir := t.TempDir()
	if err := os.WriteFile(filepath.Join(dir, "go.mod"), []byte("module example.test/missing\n\ngo 1.22\n"), 0644); err != nil {
		t.Fatal(err)
	}
	src := "package p\nimport _ \"example.invalid/not-cached\"\nfunc run() {}\n"
	if err := os.WriteFile(filepath.Join(dir, "main.go"), []byte(src), 0644); err != nil {
		t.Fatal(err)
	}
	got := analyze(request{PackageDir: dir, GoBinary: binary})
	if got.Status != "incomplete" || got.Reason != "package_load_failed" {
		t.Fatalf("missing dependency: %+v", got)
	}
}
