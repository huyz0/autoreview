package review

import "context"

func Example() {
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	_ = ctx
}
