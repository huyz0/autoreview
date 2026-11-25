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
	files    []sourceFile
	importer types.Importer
	path     string
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

func loadPackage(fset *token.FileSet, dir, binary string) (loadedPackage, string) {
	if !filepath.IsAbs(dir) || !filepath.IsAbs(binary) {
		return loadedPackage{}, "invalid_package_config"
	}
	ctx, cancel := context.WithTimeout(context.Background(), 60*time.Second)
	defer cancel()
	command := exec.CommandContext(ctx, binary, "list", "-deps", "-export", "-json", ".")
	command.Dir = dir
	command.Env = append(os.Environ(), "GOPROXY=off", "GOSUMDB=off", "GOTOOLCHAIN=local", "GOFLAGS=")
	var output limitedBuffer
	output.limit = maxListOutput
	command.Stdout = &output
	command.Stderr = io.Discard
	if command.Run() != nil {
		return loadedPackage{}, "package_load_failed"
	}
	packages, ok := decodePackages(output.Bytes())
	if !ok {
		return loadedPackage{}, "package_load_failed"
	}
	target, exports, ok := packageExports(packages, dir)
	if !ok {
		return loadedPackage{}, "package_load_incomplete"
	}
	sources, ok := readSources(target)
	if !ok {
		return loadedPackage{}, "package_sources_unavailable"
	}
	lookup := func(path string) (io.ReadCloser, error) {
		if mapped := target.ImportMap[path]; mapped != "" {
			path = mapped
		}
		return os.Open(exports[path])
	}
	return loadedPackage{files: sources, importer: importer.ForCompiler(fset, "gc", lookup), path: target.ImportPath}, ""
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
		if filepath.Base(name) != name {
			return nil, false
		}
		path := filepath.Join(target.Dir, name)
		info, err := os.Stat(path)
		if err != nil || !info.Mode().IsRegular() {
			return nil, false
		}
		size += info.Size()
		if size > maxPackageSource {
			return nil, false
		}
		data, err := os.ReadFile(path)
		if err != nil || int64(len(data)) != info.Size() {
			return nil, false
		}
		sources = append(sources, sourceFile{Path: name, Source: string(data)})
	}
	return sources, len(sources) > 0
}

func matchesExpectedSources(files []sourceFile, calls []callSite, expected map[string]string) bool {
	hashes := make(map[string]string, len(files))
	for _, file := range files {
		hash := sha256.Sum256([]byte(file.Source))
		hashes[file.Path] = hex.EncodeToString(hash[:])
	}
	for _, call := range calls {
		if expected[call.Path] == "" || expected[call.Path] != hashes[call.Path] {
			return false
		}
	}
	return true
}
