package main

import (
	"bytes"
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"go/importer"
	"go/token"
	"go/types"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"time"
)

const maxListOutput = 16 * 1024 * 1024
const maxPackageSource = 8 * 1024 * 1024

type listedPackage struct {
	Dir        string
	ImportPath string
	GoFiles    []string
	CgoFiles   []string
	Export     string
	Incomplete bool
	ImportMap  map[string]string
	Error      *struct{ Err string }
}
type loadedPackage struct {
	files        []sourceFile
	importer     types.Importer
	path         string
	dependencies []verifiedDependency
}

type verifiedDependency struct {
	listed  listedPackage
	files   []sourceFile
	exports map[string]string
}

type limitedBuffer struct {
	bytes.Buffer
	limit int
}

func (b *limitedBuffer) Write(p []byte) (int, error) {
	if len(p) > b.limit-b.Len() {
		return 0, io.ErrShortWrite
	}
	return b.Buffer.Write(p)
}

func loadPackage(fset *token.FileSet, dir, binary, mode string, snapshots map[string]dependencySnapshot) (loadedPackage, string) {
	if reason := packageConfigError(dir, binary, mode); reason != "" {
		return loadedPackage{}, reason
	}
	if mode == "" {
		mode = "readonly"
	}
	packages, ok := listPackages(dir, binary, mode)
	if !ok {
		return loadedPackage{}, "package_load_failed"
	}
	target, exports, ok := packageExports(packages, dir)
	if !ok {
		return loadedPackage{}, "package_load_incomplete"
	}
	loaded, reason := assemblePackage(fset, target, exports)
	if reason != "" {
		return loadedPackage{}, reason
	}
	dependencies, ok := verifyDependencySources(packages, target.ImportPath, snapshots, exports)
	if !ok {
		return loadedPackage{}, "dependency_source_mismatch"
	}
	loaded.dependencies = dependencies
	return loaded, ""
}

func packageConfigError(dir, binary, mode string) string {
	if !filepath.IsAbs(dir) || !filepath.IsAbs(binary) {
		return "invalid_package_config"
	}
	if mode != "" && mode != "readonly" && mode != "vendor" {
		return "invalid_module_mode"
	}
	return ""
}

func assemblePackage(fset *token.FileSet, target listedPackage, exports map[string]string) (loadedPackage, string) {
	sources, ok := readSources(target)
	if !ok {
		return loadedPackage{}, "package_sources_unavailable"
	}
	return loadedPackage{files: sources, importer: packageImporter(fset, target, exports), path: target.ImportPath}, ""
}

func packageImporter(fset *token.FileSet, target listedPackage, exports map[string]string) types.Importer {
	lookup := func(path string) (io.ReadCloser, error) {
		if mapped := target.ImportMap[path]; mapped != "" {
			path = mapped
		}
		return os.Open(exports[path])
	}
	return importer.ForCompiler(fset, "gc", lookup)
}

func listPackages(dir, binary, mode string) ([]listedPackage, bool) {
	ctx, cancel := context.WithTimeout(context.Background(), 60*time.Second)
	defer cancel()
	command := exec.CommandContext(ctx, binary, "list", "-deps", "-export", "-json", ".")
	command.Dir = dir
	command.Env = append(os.Environ(), "GOPROXY=off", "GOSUMDB=off", "GOTOOLCHAIN=local", "GOFLAGS=-mod="+mode)
	var output limitedBuffer
	output.limit = maxListOutput
	command.Stdout = &output
	command.Stderr = io.Discard
	if command.Run() != nil {
		return nil, false
	}
	return decodePackages(output.Bytes())
}

func decodePackages(data []byte) ([]listedPackage, bool) {
	decoder := json.NewDecoder(bytes.NewReader(data))
	var packages []listedPackage
	for {
		var item listedPackage
		err := decoder.Decode(&item)
		if err == io.EOF {
			return packages, len(packages) > 0
		}
		if err != nil {
			return nil, false
		}
		packages = append(packages, item)
	}
}

func packageExports(packages []listedPackage, dir string) (listedPackage, map[string]string, bool) {
	requested, err := os.Stat(dir)
	if err != nil {
		return listedPackage{}, nil, false
	}
	exports := make(map[string]string, len(packages))
	var target listedPackage
	count := 0
	for _, item := range packages {
		if item.Incomplete || item.Error != nil {
			return listedPackage{}, nil, false
		}
		if item.Export != "" {
			exports[item.ImportPath] = item.Export
		}
		info, err := os.Stat(item.Dir)
		if err == nil && os.SameFile(requested, info) {
			target = item
			count++
		}
	}
	return target, exports, count == 1 && target.ImportPath != "" && len(target.CgoFiles) == 0
}

func readSources(target listedPackage) ([]sourceFile, bool) {
	sources := make([]sourceFile, 0, len(target.GoFiles))
	size := int64(0)
	for _, name := range target.GoFiles {
		source, ok := readSource(target.Dir, name, maxPackageSource-size)
		if !ok {
			return nil, false
		}
		size += int64(len(source.Source))
		sources = append(sources, source)
	}
	return sources, len(sources) > 0
}

func readSource(dir, name string, remaining int64) (sourceFile, bool) {
	path, size, ok := sourcePath(dir, name, remaining)
	if !ok {
		return sourceFile{}, false
	}
	file, err := os.Open(path)
	if err != nil {
		return sourceFile{}, false
	}
	defer file.Close() //nolint:errcheck // Closing a read-only source cannot change the bytes read.
	data, err := io.ReadAll(io.LimitReader(file, remaining+1))
	if err != nil || int64(len(data)) != size || int64(len(data)) > remaining {
		return sourceFile{}, false
	}
	return sourceFile{Path: name, Source: string(data)}, true
}

func sourcePath(dir, name string, remaining int64) (string, int64, bool) {
	if filepath.Base(name) != name {
		return "", 0, false
	}
	path := filepath.Join(dir, name)
	info, err := os.Stat(path)
	if err != nil || !info.Mode().IsRegular() || info.Size() > remaining {
		return "", 0, false
	}
	return path, info.Size(), true
}

func matchesExpectedSources(files []sourceFile, calls []callSite, expected map[string]string, requireAll bool) bool {
	hashes := make(map[string]string, len(files))
	for _, file := range files {
		hash := sha256.Sum256([]byte(file.Source))
		hashes[file.Path] = hex.EncodeToString(hash[:])
	}
	for path, hash := range expected {
		if hash == "" || hash != hashes[path] {
			return false
		}
	}
	if requireAll && len(expected) != len(hashes) {
		return false
	}
	for _, call := range calls {
		if expected[call.Path] == "" {
			return false
		}
	}
	return true
}

// Package candidates may include files excluded by build constraints, but each
// selected file must have a matching digest. The separate expected-source map
// still checks every file that the analyzer actually loaded.
func matchesPackageSources(files []sourceFile, candidates, expected map[string]string) bool {
	if candidates == nil {
		return matchesExpectedSources(files, nil, expected, true)
	}
	for _, file := range files {
		hash := sha256.Sum256([]byte(file.Source))
		if candidates[file.Path] != hex.EncodeToString(hash[:]) {
			return false
		}
	}
	return true
}
