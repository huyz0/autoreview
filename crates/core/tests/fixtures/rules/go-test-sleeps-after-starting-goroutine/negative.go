package example

import (
	"testing"
	"time"
)

func TestWaitsForPublication(t *testing.T) {
	done := make(chan struct{})
	go publish(done)
	<-done
	if !ready.Load() {
		t.Error("publication did not finish")
	}
}

func TestDataExpires(t *testing.T) {
	store()
	time.Sleep(time.Second)
	if !expired() {
		t.Error("data did not expire")
	}
}
