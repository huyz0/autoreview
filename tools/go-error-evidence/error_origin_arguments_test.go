package main

import (
	"fmt"
	"go/ast"
	"go/constant"
	"go/parser"
	"go/token"
	"go/types"
	"strings"
	"testing"
)

func TestIntegerGuardRequiresStableParameterAndConstantOperands(t *testing.T) {
	src := `package p
func safe(n int) bool { return n > 1 }
func changed(n int) bool { n = 2; return n > 1 }
func escaped(n int) bool { _ = &n; return n > 1 }
func nestedEscape(n int) bool { _ = &(n); return n > 1 }
func incremented(n int) bool { n++; return n > 1 }
func captured(n int) bool { f := func() { n = 2 }; f(); return n > 1 }
func shadow(n int) bool { { n := 2; _ = n }; return n > 1 }
func variable(n, limit int) bool { return n > limit }
`
	fset := token.NewFileSet()
	file, err := parser.ParseFile(fset, "main.go", src, 0)
	if err != nil {
		t.Fatal(err)
	}
	info := &types.Info{Types: map[ast.Expr]types.TypeAndValue{}, Uses: map[*ast.Ident]types.Object{}, Defs: map[*ast.Ident]types.Object{}}
	if _, err := (&types.Config{}).Check("p", fset, []*ast.File{file}, info); err != nil {
		t.Fatal(err)
	}
	for _, item := range file.Decls {
		decl := item.(*ast.FuncDecl)
		function := info.Defs[decl.Name].(*types.Func)
		param := function.Type().(*types.Signature).Params().At(0)
		stable := stableIntegerParameter(decl.Body, param, info)
		wantStable := decl.Name.Name == "safe" || decl.Name.Name == "shadow" || decl.Name.Name == "variable"
		if stable != wantStable {
			t.Errorf("%s: stable=%v", decl.Name.Name, stable)
		}
		ret := decl.Body.List[len(decl.Body.List)-1].(*ast.ReturnStmt)
		value, known := integerGuard(ret.Results[0], param, constant.MakeInt64(1), info)
		if decl.Name.Name == "variable" {
			if known {
				t.Error("variable threshold should be unknown")
			}
		} else if !known || value {
			t.Errorf("%s: guard=%v known=%v", decl.Name.Name, value, known)
		}
	}
}

func TestSpecializationBudgetLeavesLaterCallsUnknown(t *testing.T) {
	var src strings.Builder
	src.WriteString("package p\nimport (\"context\"; \"errors\")\nfunc guarded(ctx context.Context, n int) error { if n > 1000 { return errors.New(\"other\") }; return ctx.Err() }\nfunc run(ctx context.Context) {\n")
	var snippets []string
	for i := 0; i < 257; i++ {
		call := fmt.Sprintf("guarded(ctx, %d)", i)
		snippets = append(snippets, call)
		fmt.Fprintf(&src, "%s\n", call)
	}
	src.WriteString("}\n")
	got := analyzeOrigins(t, src.String(), snippets)
	if got.Calls[255].ErrorOrigin == nil || got.Calls[256].ErrorOrigin != nil {
		t.Fatalf("budget boundary: %+v %+v", got.Calls[255], got.Calls[256])
	}
}

func TestErrorOriginsSpecializeConstantIntegerArguments(t *testing.T) {
	src := `package p
import ("context"; "errors")
type worker struct{ limit int }
func guarded(ctx context.Context, n int) error { if n > 1 { return errors.New("other") }; return ctx.Err() }
func (worker) method(ctx context.Context, n int) error { if n > 1 { return errors.New("other") }; return ctx.Err() }
func (w worker) field(ctx context.Context, n int) error { if n > w.limit { return errors.New("other") }; return ctx.Err() }
func changed(ctx context.Context, n int) error { n++; if n > 1 { return errors.New("other") }; return ctx.Err() }
func escaped(ctx context.Context, n int) error { _ = &n; if n > 1 { return errors.New("other") }; return ctx.Err() }
func run(ctx context.Context, n int, w worker) { guarded(ctx, 1); guarded(ctx, 2); guarded(ctx, n); worker.method(w, ctx, 1); changed(ctx, 1); escaped(ctx, 1); w.field(ctx, 1) }
`
	snippets := []string{"guarded(ctx, 1);", "guarded(ctx, 2);", "guarded(ctx, n);", "worker.method(w, ctx, 1);", "changed(ctx, 1);", "escaped(ctx, 1);", "w.field(ctx, 1)"}
	got := analyzeOrigins(t, src, snippets)
	t.Run("specialized calls", func(t *testing.T) { assertErrorOrigins(t, got, snippets, []int{0, -1, -1, 0, -1, -1, -1}) })
	for _, i := range []int{0, 3} {
		origin := got.Calls[i].ErrorOrigin
		if origin == nil || origin.Specialized == nil || origin.Specialized.Parameter != 1 || origin.Specialized.Value != "1" {
			t.Errorf("specialization for %s: %+v", snippets[i], origin)
		}
	}
}

func TestSpecializationOffsetsParenthesizedMethodExpressions(t *testing.T) {
	src := `package p
import ("context"; "errors")
type count int
func (count) guarded(n int, ctx context.Context) error { if n > 1 { return errors.New("other") }; return ctx.Err() }
func run(ctx context.Context) { (count.guarded)(1, 2, ctx); (count.guarded)(2, 1, ctx) }
`
	snippets := []string{"(count.guarded)(1, 2, ctx);", "(count.guarded)(2, 1, ctx)"}
	got := analyzeOrigins(t, src, snippets)
	t.Run("receiver is not an argument", func(t *testing.T) { assertErrorOrigins(t, got, snippets, []int{-1, 1}) })
	if got.Calls[1].ErrorOrigin == nil || got.Calls[1].ErrorOrigin.Specialized == nil || got.Calls[1].ErrorOrigin.Specialized.Parameter != 0 {
		t.Fatalf("parameter index: %+v", got.Calls[1])
	}
}
