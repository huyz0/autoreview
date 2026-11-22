package flow

import "slices"

func f(items, allowed []string) {
	for _, item := range items {
		println(item)
	}
	if slices.Contains(allowed, "one") {
		println("yes")
	}
}

func deferred(items, allowed []string) []func() bool {
	var callbacks []func() bool
	for _, item := range items {
		callbacks = append(callbacks, func() bool {
			return slices.Contains(allowed, item)
		})
	}
	return callbacks
}
