package example

import (
	"net/http"
	"strings"
)

func create(url string) (*http.Response, error) {
	for {
		res, err := http.Post(url, "text/plain", strings.NewReader("payload"))
		if err == nil {
			return res, nil
		}
	}
}
