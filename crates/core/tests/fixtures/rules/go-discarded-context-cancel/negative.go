package review

import "context"

func QueryWithCancel() {
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	_ = ctx
}
