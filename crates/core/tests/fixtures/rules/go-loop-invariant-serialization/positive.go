package main
import "encoding/json"

func f(config Config, items []int) {
	for i := 0; i < len(items); i++ {
		b, err := json.Marshal(config)
		_ = b
		_ = err
	}
}
