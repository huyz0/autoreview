package main

import (
	"log"
	"os"
)

func reportSafeStartup() {
	log.Println("region", os.Getenv("REGION"))
	log.Println("credential configured", os.Getenv("SERVICE_API_KEY") != "")
}
