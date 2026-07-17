package main
import "regexp"

func f(items []string) {
	re := regexp.MustCompile(`^[a-z]+$`)
	for i := 0; i < len(items); i++ {
		_ = re
	}
}
