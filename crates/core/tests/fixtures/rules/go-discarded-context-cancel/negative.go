package review

import "context"

func queryWithCancel() {
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	_ = ctx
}
