package example

import (
	"net/http"
	"strings"
)

func once(url string) (*http.Response, error) {
	return http.Post(url, "text/plain", strings.NewReader("payload"))
}
