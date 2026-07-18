// Tier 4 real-semantic backend for autoreview-symindex's Feature Envy
// query (see SESSION_NOTES.md follow-up #3). Loads and type-checks the
// target repo's Go packages with go/packages, then for every method walks
// its body and resolves each selector expression's receiver to its real
// static type — a genuine type-checked answer to "is this access foreign,
// and to which type", instead of symindex's own tree-sitter-only
// name-guessing heuristic. Emits one JSON object per line (JSONL) on
// stdout so the Rust side can stream-parse without buffering a whole-repo
// result in memory.
//
// Deliberately narrow: this only reports resolved accesses, one per
// selector expression on a method receiver's own body. It does not
// attempt to reproduce symindex's Feature Envy scoring itself — that
// stays in Rust, so the scoring/threshold logic isn't duplicated across
// two languages.
package main

import (
	"bufio"
	"encoding/json"
	"fmt"
	"go/ast"
	"go/types"
	"os"

	"golang.org/x/tools/go/packages"
)

type accessRecord struct {
	File          string `json:"file"`
	Line          int    `json:"line"`
	Method        string `json:"method"`
	ReceiverType  string `json:"receiver_type"`
	AccessedIdent string `json:"accessed_ident"`
	AccessedType  string `json:"accessed_type"`
}

func namedTypeName(t types.Type) string {
	// Peel off pointers so *Foo and Foo are treated as the same named type.
	for {
		if p, ok := t.(*types.Pointer); ok {
			t = p.Elem()
			continue
		}
		break
	}
	if n, ok := t.(*types.Named); ok {
		obj := n.Obj()
		if obj.Pkg() != nil {
			return obj.Pkg().Path() + "." + obj.Name()
		}
		return obj.Name()
	}
	return ""
}

func receiverTypeName(recv *types.Var) string {
	return namedTypeName(recv.Type())
}

func main() {
	if len(os.Args) < 2 {
		fmt.Fprintln(os.Stderr, "usage: tier4-go <repo-dir> [package-pattern]")
		os.Exit(2)
	}
	repoDir := os.Args[1]
	pattern := "./..."
	if len(os.Args) >= 3 {
		pattern = os.Args[2]
	}

	cfg := &packages.Config{
		Mode: packages.NeedName | packages.NeedFiles | packages.NeedSyntax |
			packages.NeedTypes | packages.NeedTypesInfo,
		Dir:   repoDir,
		Tests: false,
	}
	pkgs, err := packages.Load(cfg, pattern)
	if err != nil {
		fmt.Fprintln(os.Stderr, "tier4-go: load failed:", err)
		os.Exit(1)
	}
	// A repo that doesn't build cleanly still gets partial results from
	// go/packages (best-effort type info) — errors are reported per-package
	// to stderr for visibility but don't abort the whole run, since a
	// single broken package shouldn't blank out results for the rest.
	hadErrors := false
	packages.Visit(pkgs, nil, func(pkg *packages.Package) {
		for _, e := range pkg.Errors {
			fmt.Fprintln(os.Stderr, "tier4-go:", e)
			hadErrors = true
		}
	})

	w := bufio.NewWriter(os.Stdout)
	defer w.Flush()
	enc := json.NewEncoder(w)

	for _, pkg := range pkgs {
		if pkg.TypesInfo == nil {
			continue
		}
		fset := pkg.Fset
		for _, file := range pkg.Syntax {
			ast.Inspect(file, func(n ast.Node) bool {
				fn, ok := n.(*ast.FuncDecl)
				if !ok || fn.Recv == nil || len(fn.Recv.List) == 0 {
					return true
				}
				recvObj := pkg.TypesInfo.Defs[fn.Recv.List[0].Names[0]]
				recvVar, ok := recvObj.(*types.Var)
				if !ok {
					return true
				}
				receiverType := receiverTypeName(recvVar)
				if receiverType == "" {
					return true
				}

				ast.Inspect(fn.Body, func(bn ast.Node) bool {
					sel, ok := bn.(*ast.SelectorExpr)
					if !ok {
						return true
					}
					// sel.X's *static type* is what matters, whatever shape
					// the expression is — a bare identifier (`s`), a field
					// chain (`s.other`), or a call result. This is what lets
					// a chained foreign access like `s.other.Get()` resolve
					// to `Other` for the `.Get` selector, not just direct
					// one-hop accesses off a local variable.
					tv, ok := pkg.TypesInfo.Types[sel.X]
					if !ok || tv.Type == nil {
						return true
					}
					accessedType := namedTypeName(tv.Type)
					if accessedType == "" {
						return true
					}
					pos := fset.Position(sel.Pos())
					_ = enc.Encode(accessRecord{
						File:          pos.Filename,
						Line:          pos.Line,
						Method:        fn.Name.Name,
						ReceiverType:  receiverType,
						AccessedIdent: sel.Sel.Name,
						AccessedType:  accessedType,
					})
					return true
				})
				return true
			})
		}
	}

	if hadErrors {
		os.Exit(0) // partial results are still useful; exit 0 so the caller reads stdout
	}
}
