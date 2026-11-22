package collections

import "slices"

func removeFirst(items []*string) {
	slices.Delete(items, 0, 1)
}
