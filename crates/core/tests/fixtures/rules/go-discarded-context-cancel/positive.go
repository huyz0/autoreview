package review

import "context"

func newQueryContext() context.Context {
	ctx, _ := context.WithCancel(context.Background())
	return ctx
}
