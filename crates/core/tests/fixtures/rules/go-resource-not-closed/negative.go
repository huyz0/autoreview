package main
import "os"

func f() {
	f, err := os.Open("a.txt")
	if err != nil {
		return
	}
	defer f.Close()
}
