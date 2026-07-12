package main

import "crypto/tls"

func main() {
	cfg := &tls.Config{MinVersion: tls.VersionTLS12, InsecureSkipVerify: true}
	_ = cfg
}
