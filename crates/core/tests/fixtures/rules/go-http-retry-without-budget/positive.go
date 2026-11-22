package example

import "net/http"

func Get(url string) (*http.Response, error) {
	for {
		response, err := http.Get(url)
		if err == nil {
			return response, nil
		}
	}
}
