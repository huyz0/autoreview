package flow

import "slices"

func f(items, allowed []string) {
	for _, item := range items {
		if slices.Contains(allowed, item) {
			println(item)
		}
	}
}
