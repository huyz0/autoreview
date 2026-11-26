package main

import (
	"fmt"
	"strings"
	"testing"
)

func TestContextErrorOriginsRequireProvenReturnsAndStableParameter(t *testing.T) {
	src := `package p
import ("context"; "errors")
func safe(ctx context.Context, ok bool) error { if ok { return nil }; <-ctx.Done(); return ctx.Err() }
func direct(ctx context.Context) error { return ctx.Err() }
func other(ctx context.Context) error { if ctx.Err() != nil { return ctx.Err() }; return errors.New("other") }
func reassigned(ctx context.Context, other context.Context) error { ctx = other; return ctx.Err() }
func escaped(ctx context.Context) error { mutate(&ctx); return ctx.Err() }
func mutate(*context.Context) {}
func named(ctx context.Context) (err error) { return ctx.Err() }
func deferred(ctx context.Context) error { defer func(){}(); return ctx.Err() }
func wrapped(ctx context.Context) error { return errors.Join(ctx.Err()) }
func shadowNil(ctx context.Context) error { nil := errors.New("other"); if ctx.Err() != nil { return ctx.Err() }; return nil }
func run(ctx context.Context) { safe(ctx, true); direct(ctx); other(ctx); reassigned(ctx, ctx); escaped(ctx); named(ctx); deferred(ctx); wrapped(ctx); shadowNil(ctx) }
`
	snippets := []string{"safe(ctx, true)", "direct(ctx)", "other(ctx)", "reassigned(ctx, ctx)", "escaped(ctx)", "named(ctx)", "deferred(ctx)", "wrapped(ctx)", "shadowNil(ctx)"}
	got := analyzeOrigins(t, src, snippets)
	t.Run("stable returns", func(t *testing.T) {
		assertErrorOrigins(t, got, snippets, []int{0, 0, -1, -1, -1, -1, -1, -1, -1})
	})
}

func TestContextErrorSummaryPassLimitLeavesLongChainsUnknown(t *testing.T) {
	var src strings.Builder
	src.WriteString("package p\nimport \"context\"\n")
	for i := 0; i < 19; i++ {
		fmt.Fprintf(&src, "func f%d(ctx context.Context) error { return f%d(ctx) }\n", i, i+1)
	}
	src.WriteString("func f19(ctx context.Context) error { return ctx.Err() }\nfunc run(ctx context.Context) { f0(ctx); f4(ctx) }\n")
	text := src.String()
	req := callsInSource(t, text, []string{"f0(ctx);", "f4(ctx) }"}, false)
	req.IncludeErrorOrigins = true
	got := analyze(req)
	if got.Status != "ok" || len(got.Calls) != 2 {
		t.Fatalf("result: %+v", got)
	}
	if got.Calls[0].ErrorOrigin != nil {
		t.Fatalf("chain beyond budget: %+v", got.Calls[0])
	}
	if got.Calls[1].ErrorOrigin == nil {
		t.Fatalf("chain within budget: %+v", got.Calls[1])
	}
}

func TestErrorOriginDoesNotTrustUnrelatedContextOrNil(t *testing.T) {
	src := `package p
type Context interface { Err() error; Done() <-chan struct{} }
func fake(ctx Context) error { return ctx.Err() }
func use(ctx Context) { fake(ctx) }
`
	req := request{Files: []sourceFile{{"main.go", src}}, Calls: []callSite{{"main.go", strings.LastIndex(src, "fake(ctx)")}}, IncludeErrorOrigins: true}
	got := analyze(req)
	if got.Status != "ok" || got.Calls[0].ErrorOrigin != nil {
		t.Fatalf("unrelated context: %+v", got)
	}
}

func TestContextErrorOriginsThroughHelpers(t *testing.T) {
	src := `package p
import ("context"; "errors")
type worker struct{}
func outer(ctx context.Context) error { return middle(ctx) }
func middle(ctx context.Context) error { return leaf(ctx) }
func leaf(ctx context.Context) error { return ctx.Err() }
func (worker) method(ctx context.Context) error { return leaf(ctx) }
func expression(w worker, ctx context.Context) error { return worker.method(w, ctx) }
func unrelated(ctx context.Context) error { if ctx.Err() != nil { return leaf(ctx) }; return errors.New("other") }
func otherContext(ctx, other context.Context) error { return leaf(other) }
func dynamic(ctx context.Context, f func(context.Context) error) error { return f(ctx) }
func cyclic(ctx context.Context) error { return cycle(ctx) }
func cycle(ctx context.Context) error { return cyclic(ctx) }
func run(ctx context.Context, w worker, f func(context.Context) error) { outer(ctx); w.method(ctx); expression(w, ctx); unrelated(ctx); otherContext(ctx, ctx); dynamic(ctx, f); cyclic(ctx) }
`
	snippets := []string{"outer(ctx);", "w.method(ctx);", "expression(w, ctx);", "unrelated(ctx);", "otherContext(ctx, ctx);", "dynamic(ctx, f);", "cyclic(ctx) }"}
	got := analyzeOrigins(t, src, snippets)
	t.Run("helper forwarding", func(t *testing.T) {
		assertErrorOrigins(t, got, snippets, []int{0, 0, 1, -1, 1, -1, -1})
	})
}

func assertErrorOrigins(t *testing.T, got response, snippets []string, expected []int) {
	t.Helper()
	for i, call := range got.Calls {
		if expected[i] < 0 {
			if call.ErrorOrigin != nil {
				t.Errorf("unproven %s: %+v", snippets[i], call)
			}
			continue
		}
		if call.ErrorOrigin == nil || call.ErrorOrigin.Kind != "context_error_or_nil" || call.ErrorOrigin.ContextParameter != expected[i] {
			t.Errorf("%s: %+v", snippets[i], call)
		}
	}
}

func analyzeOrigins(t *testing.T, src string, snippets []string) response {
	t.Helper()
	req := callsInSource(t, src, snippets, false)
	req.IncludeErrorOrigins = true
	got := analyze(req)
	if got.Status != "ok" || len(got.Calls) != len(snippets) {
		t.Fatalf("result: %+v", got)
	}
	return got
}

func TestContextErrorOriginsThroughCheckedBindings(t *testing.T) {
	src := `package p
import ("context"; "errors")
func leaf(ctx context.Context) error { return ctx.Err() }
func plain(ctx context.Context) error { err := leaf(ctx); if err != nil { return err }; return nil }
func inIf(ctx context.Context) error { if err := leaf(ctx); err != nil { return err }; return nil }
func overwrite(ctx context.Context) error { err := leaf(ctx); err = errors.New("other"); return err }
func escaped(ctx context.Context) error { err := leaf(ctx); consume(&err); return err }
func consume(*error) {}
func changed(ctx context.Context) error { err := leaf(ctx); ctx = context.Background(); return err }
func shadowed(ctx context.Context, flag bool) error { err := leaf(ctx); if flag { err := errors.New("other"); return err }; return err }
func run(ctx context.Context) { plain(ctx); inIf(ctx); overwrite(ctx); escaped(ctx); changed(ctx); shadowed(ctx, true) }
`
	snippets := []string{"plain(ctx);", "inIf(ctx);", "overwrite(ctx);", "escaped(ctx);", "changed(ctx);", "shadowed(ctx, true)"}
	got := analyzeOrigins(t, src, snippets)
	t.Run("checked bindings", func(t *testing.T) { assertErrorOrigins(t, got, snippets, []int{0, 0, -1, -1, -1, -1}) })
}

func TestContextErrorOriginsIgnoreOnlyConstantUnreachableBranches(t *testing.T) {
	src := `package p
import ("context"; "errors")
func falseBranch(ctx context.Context) error { if 1 > 1 { return errors.New("other") }; return ctx.Err() }
func trueBranch(ctx context.Context) error { if true { return ctx.Err() } else { return errors.New("other") } }
func runtimeBranch(ctx context.Context, n int) error { if n > 1 { return errors.New("other") }; return ctx.Err() }
func initRuns(ctx context.Context) error { if ctx = context.Background(); false { return nil }; return ctx.Err() }
func deadMutation(ctx context.Context) error { if false { ctx = context.Background() }; return ctx.Err() }
func run(ctx context.Context, n int) { falseBranch(ctx); trueBranch(ctx); runtimeBranch(ctx,n); initRuns(ctx); deadMutation(ctx) }
`
	snippets := []string{"falseBranch(ctx);", "trueBranch(ctx);", "runtimeBranch(ctx,n);", "initRuns(ctx);", "deadMutation(ctx)"}
	got := analyzeOrigins(t, src, snippets)
	t.Run("constant guards", func(t *testing.T) { assertErrorOrigins(t, got, snippets, []int{0, 0, -1, -1, 0}) })
}
