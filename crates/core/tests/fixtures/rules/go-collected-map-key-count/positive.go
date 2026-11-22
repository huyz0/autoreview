package collections

import (
	"maps"
	"slices"
)

func countRegions(values map[string]int) int {
	return len(slices.Collect(maps.Keys(values)))
}
