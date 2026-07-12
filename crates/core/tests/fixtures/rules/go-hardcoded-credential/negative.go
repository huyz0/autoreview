package main

import "os"

func main() {
	password := os.Getenv("PASSWORD")
	_ = password
}
