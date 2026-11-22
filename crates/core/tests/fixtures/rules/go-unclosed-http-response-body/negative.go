package main
import "net/http"

func Forward(url string) (*http.Response, error) {
	resp, err := http.Get(url)
	if err == nil {
		return resp, nil
	}
	return nil, err
}

func f() {
	resp, err := http.Get("http://example.com")
	if err != nil {
		return
	}
	defer resp.Body.Close()
}
