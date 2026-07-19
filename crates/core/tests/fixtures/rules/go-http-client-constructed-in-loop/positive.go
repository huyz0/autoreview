package main

import "net/http"

func fetchAll(urls []string) {
	for _, u := range urls {
		client := &http.Client{}
		client.Get(u)
	}
}
