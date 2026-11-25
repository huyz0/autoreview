// go-error-evidence is an experimental, offline type-evidence probe.
// Its caller must provide the complete set of files selected by the Go build.
package main

import (
	"bytes"
	"encoding/json"
	"fmt"
	"go/ast"
	"go/importer"
	"go/parser"
	"go/token"
	"go/types"
	"io"
	"os"
)

const maxInput = 1024 * 1024

type sourceFile struct {
	Path   string `json:"path"`
	Source string `json:"source"`
}
type callSite struct {
	Path  string `json:"path"`
	Start int    `json:"start"`
}
type request struct {
	Files []sourceFile `json:"files"`
	Calls []callSite   `json:"calls"`
}
type callResult struct {
	Path   string `json:"path"`
	Start  int    `json:"start"`
	Status string `json:"status"`
}
type response struct {
	Status string       `json:"status"`
	Reason string       `json:"reason,omitempty"`
	Calls  []callResult `json:"calls,omitempty"`
}

func analyze(req request) response {
	if len(req.Files) == 0 {
		return incomplete("no_files")
	}
	fset := token.NewFileSet()
	files := make([]*ast.File, 0, len(req.Files))
	byPath := make(map[string]*ast.File, len(req.Files))
	sourceByPath := make(map[string]string, len(req.Files))
	for _, file := range req.Files {
		if file.Path == "" || byPath[file.Path] != nil {
			return incomplete("invalid_files")
		}
		parsed, err := parser.ParseFile(fset, file.Path, file.Source, 0)
		if err != nil {
			return incomplete("syntax_errors")
		}
		byPath[file.Path] = parsed
		sourceByPath[file.Path] = file.Source
		files = append(files, parsed)
	}
	info := &types.Info{Types: make(map[ast.Expr]types.TypeAndValue)}
	config := types.Config{Importer: importer.Default(), Error: func(error) {}}
	if _, err := config.Check("autoreview.local/package", fset, files, info); err != nil {
		return incomplete("type_errors")
	}
	indexes := make(map[string]map[int]*ast.CallExpr, len(files))
	for path, file := range byPath {
		calls := make(map[int]*ast.CallExpr)
		ast.Inspect(file, func(node ast.Node) bool {
			if call, ok := node.(*ast.CallExpr); ok {
				calls[fset.Position(call.Pos()).Offset] = call
			}
			return true
		})
		indexes[path] = calls
	}
	result := response{Status: "ok", Calls: make([]callResult, 0, len(req.Calls))}
	builtinError := types.Universe.Lookup("error").Type()
	for _, call := range req.Calls {
		status := "unavailable"
		if call.Start >= 0 && call.Start < len(sourceByPath[call.Path]) {
			if node := indexes[call.Path][call.Start]; node != nil {
				status = "not_proven"
				funType := info.TypeOf(node.Fun)
				if funType != nil {
					signature, ok := funType.Underlying().(*types.Signature)
					if ok && signature.Results().Len() == 1 && types.Identical(signature.Results().At(0).Type(), builtinError) {
						status = "error"
					}
				}
			}
		}
		result.Calls = append(result.Calls, callResult{Path: call.Path, Start: call.Start, Status: status})
	}
	return result
}

func incomplete(reason string) response { return response{Status: "incomplete", Reason: reason} }

func main() {
	data, err := io.ReadAll(io.LimitReader(os.Stdin, maxInput+1))
	if err != nil || len(data) > maxInput {
		fmt.Fprintln(os.Stderr, "invalid input")
		os.Exit(1)
	}
	decoder := json.NewDecoder(bytes.NewReader(data))
	decoder.DisallowUnknownFields()
	var req request
	if decoder.Decode(&req) != nil || decoder.Decode(new(any)) != io.EOF {
		fmt.Fprintln(os.Stderr, "invalid request")
		os.Exit(1)
	}
	if err := json.NewEncoder(os.Stdout).Encode(analyze(req)); err != nil {
		os.Exit(1)
	}
}
