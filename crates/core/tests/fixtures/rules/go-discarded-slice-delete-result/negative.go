package collections

import "slices"

func unique(values []string) []string {
	return slices.Compact(values)
}
