package example

import "net/http"

func Unbounded(url string) (*http.Response, error) {
	client := &http.Client{Timeout: 0}
	return client.Get(url)
}
