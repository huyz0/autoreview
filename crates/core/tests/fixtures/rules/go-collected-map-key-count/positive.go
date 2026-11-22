package collections

import (
	"maps"
	"slices"
)

func CountRegions(values map[string]int) int {
	return len(slices.Collect(maps.Keys(values)))
}
