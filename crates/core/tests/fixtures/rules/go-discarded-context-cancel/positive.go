package review

import "context"

func Example() context.Context {
	ctx, _ := context.WithCancel(context.Background())
	return ctx
}
