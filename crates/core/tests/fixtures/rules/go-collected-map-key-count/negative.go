package collections

import (
	"maps"
	"slices"
)

func regions(values map[string]int) []string {
	return slices.Collect(maps.Keys(values))
}

func count(values map[string]int) int {
	return len(values)
}
