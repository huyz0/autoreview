package main
import "regexp"

func f(items []string) {
	for i := 0; i < len(items); i++ {
		re := regexp.MustCompile(`^[a-z]+$`)
		_ = re
	}
}
