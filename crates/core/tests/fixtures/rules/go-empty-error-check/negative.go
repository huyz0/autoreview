package main

import "fmt"

func doIt() error { return nil }

func main() {
	if err := doIt(); err != nil {
		fmt.Println(err)
	}
}
