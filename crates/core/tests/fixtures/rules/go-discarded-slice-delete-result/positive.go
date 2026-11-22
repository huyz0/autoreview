package collections

import "slices"

func RemoveFirst(items []*string) {
	slices.Delete(items, 0, 1)
}
