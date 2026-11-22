package collections

import "slices"

func Unique(values []string) []string {
	return slices.Compact(values)
}
