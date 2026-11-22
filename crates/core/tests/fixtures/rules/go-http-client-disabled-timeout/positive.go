package example

import "net/http"

func unbounded(url string) (*http.Response, error) {
	client := &http.Client{Timeout: 0}
	return client.Get(url)
}
