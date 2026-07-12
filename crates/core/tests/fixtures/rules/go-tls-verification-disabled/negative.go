package main

import "crypto/tls"

func main() {
	cfg := &tls.Config{MinVersion: tls.VersionTLS12, InsecureSkipVerify: false}
	_ = cfg
}
