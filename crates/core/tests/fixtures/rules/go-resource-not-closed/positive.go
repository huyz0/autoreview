package main
import "os"

func f() {
	f, err := os.Open("a.txt")
	_ = err
	_ = f
}
