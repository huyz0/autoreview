package main

import (
	"log"
	"os"
)

func report() {
	log.Printf("startup failed: %s", os.Getenv("SERVICE_API_KEY"))
}
