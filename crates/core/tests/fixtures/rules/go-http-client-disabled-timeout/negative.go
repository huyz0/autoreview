package example

import (
	"context"
	"net/http"
	"time"
)

func Bounded(url string) (*http.Response, error) {
	client := &http.Client{Timeout: time.Second}
	return client.Get(url)
}

func Request(ctx context.Context, url string) (*http.Response, error) {
	req, err := http.NewRequestWithContext(ctx, "GET", url, nil)
	if err != nil {
		return nil, err
	}
	client := &http.Client{Timeout: 0}
	return client.Do(req)
}
