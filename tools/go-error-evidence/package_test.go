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
	binary := goBinary(t)
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
	write("dep/dep.go", "package dep\nfunc Operation() error { return nil }\ntype Worker struct{}\nfunc (Worker) Wait() error { return nil }\n")
	src := "package sample\nimport alias \"example.test/probe/dep\"\nfunc run(w alias.Worker) { alias.Operation(); w.Wait() }\n"
	write("main.go", src)
	write("ignored.go", "//go:build never\n\npackage sample\nthis is not Go\n")
	sum := sha256.Sum256([]byte(src))
	req := request{PackageDir: dir, GoBinary: binary, ExpectedSources: map[string]string{"main.go": hex.EncodeToString(sum[:])}, Calls: []callSite{{"main.go", strings.Index(src, "alias.Operation()")}, {"main.go", strings.Index(src, "w.Wait()")}}, IncludeCallees: true}
	got := analyze(req)
	if got.Status != "ok" || len(got.Calls) != 2 || got.Calls[0].Status != "error" {
		t.Fatalf("package evidence: %+v", got)
	}
	if got.Calls[0].Callee == nil || *got.Calls[0].Callee != (callee{Package: "example.test/probe/dep", Name: "Operation"}) ||
		got.Calls[1].Callee == nil || *got.Calls[1].Callee != (callee{Package: "example.test/probe/dep", Name: "Wait", Receiver: "Worker"}) {
		t.Fatalf("package callee evidence: %+v", got.Calls)
	}
	req.ExpectedSources["main.go"] = strings.Repeat("0", 64)
	if got := analyze(req); got.Status != "incomplete" || got.Reason != "source_mismatch" {
		t.Fatalf("changed source: %+v", got)
	}
	req.ExpectedSources["main.go"] = hex.EncodeToString(sum[:])
	write("other.go", "package sample\nfunc helper() {}\n")
	assertSourceMismatch(t, req, "callee evidence requires hashes for all selected files")
	otherSum := sha256.Sum256([]byte("package sample\nfunc helper() {}\n"))
	req.ExpectedSources["other.go"] = hex.EncodeToString(otherSum[:])
	if got := analyze(req); got.Status != "ok" {
		t.Fatalf("complete package hashes: %+v", got)
	}
	delete(req.ExpectedSources, "other.go")
	req.PackageSources = map[string]string{"main.go": hex.EncodeToString(sum[:]), "other.go": hex.EncodeToString(otherSum[:]), "ignored.go": "not-selected"}
	if got := analyze(req); got.Status != "ok" {
		t.Fatalf("selected files can be verified among extra candidates: %+v", got)
	}
	delete(req.PackageSources, "other.go")
	assertSourceMismatch(t, req, "missing selected candidate")
	req.PackageSources = nil
	req.ExpectedSources["other.go"] = strings.Repeat("0", 64)
	if got := analyze(req); got.Status != "incomplete" || got.Reason != "source_mismatch" {
		t.Fatalf("changed helper source: %+v", got)
	}
	delete(req.ExpectedSources, "other.go")
	req.IncludeCallees = false
	if got := analyze(req); got.Status != "ok" {
		t.Fatalf("legacy call-site hash requirement: %+v", got)
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
	binary := goBinary(t)
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

func goBinary(t *testing.T) string {
	t.Helper()
	binary, err := exec.LookPath("go")
	if err != nil {
		t.Skip("Go toolchain unavailable")
	}
	binary, err = filepath.Abs(binary)
	if err != nil {
		t.Fatal(err)
	}
	return binary
}

func assertSourceMismatch(t *testing.T, req request, label string) {
	t.Helper()
	if got := analyze(req); got.Status != "incomplete" || got.Reason != "source_mismatch" {
		t.Fatalf("%s: %+v", label, got)
	}
}

func TestPackageModeLoadsVendoredDependenciesOffline(t *testing.T) {
	binary := goBinary(t)
	dir := t.TempDir()
	files := map[string]string{
		"go.mod":             "module example.test/vendorprobe\n\ngo 1.22\n\nrequire example.invalid/offline-dep v1.0.0\n",
		"vendor/modules.txt": "# example.invalid/offline-dep v1.0.0\n## explicit; go 1.22\nexample.invalid/offline-dep\n",
		"vendor/example.invalid/offline-dep/dep.go": "package dep\nfunc Operation() error { return nil }\n",
		"main.go": "package sample\nimport dep \"example.invalid/offline-dep\"\nfunc run() { dep.Operation() }\n",
	}
	for name, data := range files {
		path := filepath.Join(dir, name)
		if err := os.MkdirAll(filepath.Dir(path), 0755); err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(path, []byte(data), 0644); err != nil {
			t.Fatal(err)
		}
	}
	src := files["main.go"]
	sum := sha256.Sum256([]byte(src))
	req := request{PackageDir: dir, GoBinary: binary, ExpectedSources: map[string]string{"main.go": hex.EncodeToString(sum[:])}, Calls: []callSite{{"main.go", strings.Index(src, "dep.Operation()")}}, ModuleMode: "vendor", IncludeCallees: true}
	got := analyze(req)
	if got.Status != "ok" || len(got.Calls) != 1 || got.Calls[0].Status != "error" || got.Calls[0].Callee == nil || got.Calls[0].Callee.Package != "example.invalid/offline-dep" {
		t.Fatalf("vendor evidence: %+v", got)
	}
	req.ModuleMode = "readonly"
	if got := analyze(req); got.Status != "incomplete" || got.Reason != "package_load_failed" {
		t.Fatalf("offline readonly unexpectedly succeeded: %+v", got)
	}
	req.ModuleMode = "unexpected"
	if got := analyze(req); got.Status != "incomplete" || got.Reason != "invalid_module_mode" {
		t.Fatalf("invalid mode: %+v", got)
	}
}
