package example

import "net/http"

func bounded(url string) (*http.Response, error) {
	var last error
	for i := 0; i < 3; i++ {
		response, err := http.Get(url)
		if err == nil {
			return response, nil
		}
		last = err
	}
	return nil, last
}
