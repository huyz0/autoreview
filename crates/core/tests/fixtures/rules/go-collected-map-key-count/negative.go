package collections

import (
	"maps"
	"slices"
)

func Regions(values map[string]int) []string {
	return slices.Collect(maps.Keys(values))
}

func Count(values map[string]int) int {
	return len(values)
}
