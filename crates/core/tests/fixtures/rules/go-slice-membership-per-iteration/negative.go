package flow

import "slices"

func containsOne(allowed []string) bool {
	return slices.Contains(allowed, "one")
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
