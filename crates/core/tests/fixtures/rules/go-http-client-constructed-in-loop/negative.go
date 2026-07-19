package main

import "net/http"

func fetchAll(urls []string) {
	client := &http.Client{}
	for _, u := range urls {
		client.Get(u)
	}
}
