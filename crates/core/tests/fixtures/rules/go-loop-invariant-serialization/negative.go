package main
import "encoding/json"

func f(config Config, items []int) {
	b, err := json.Marshal(config)
	_ = b
	_ = err
	for i := 0; i < len(items); i++ {
	}
}
