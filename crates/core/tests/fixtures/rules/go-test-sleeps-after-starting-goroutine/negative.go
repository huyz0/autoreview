package example

import (
	"testing"
	"time"
)

func TestWaitsForPublication(t *testing.T) {
	done := make(chan int, 1)
	go func() { done <- 42 }()
	result := <-done
	if result != 42 {
		t.Error("publication did not finish")
	}
}

func TestDataExpires(t *testing.T) {
	expires := time.Now().Add(time.Millisecond)
	time.Sleep(time.Second)
	if time.Now().Before(expires) {
		t.Error("data did not expire")
	}
}
