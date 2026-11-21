package review

import "context"

func NewQueryContext() context.Context {
	ctx, _ := context.WithCancel(context.Background())
	return ctx
}
