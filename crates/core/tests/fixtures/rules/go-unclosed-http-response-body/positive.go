package main
import "net/http"

func f() {
	resp, err := http.Get("http://example.com")
	_ = err
	_ = resp
}
