package main

import (
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
	req := callsInSource(t, src, snippets, false)
	req.IncludeErrorOrigins = true
	got := analyze(req)
	if got.Status != "ok" || len(got.Calls) != len(snippets) {
		t.Fatalf("result: %+v", got)
	}
	for i, call := range got.Calls {
		if i < 2 {
			if call.ErrorOrigin == nil || call.ErrorOrigin.Kind != "context_error_or_nil" || call.ErrorOrigin.ContextParameter != 0 {
				t.Errorf("%s: %+v", snippets[i], call)
			}
		} else if call.ErrorOrigin != nil {
			t.Errorf("unproven %s: %+v", snippets[i], call)
		}
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
