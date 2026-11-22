package example

import "net/http"

func PostOnce(url string) (*http.Response, error) {
	return http.Post(url, "text/plain", nil)
}
