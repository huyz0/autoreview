package main
import "strings"

func f(items []string) string {
	var b strings.Builder
	for i := 0; i < len(items); i++ {
		b.WriteString(items[i])
	}
	return b.String()
}
