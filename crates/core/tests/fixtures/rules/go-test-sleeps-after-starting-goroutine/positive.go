package example

import (
	"testing"
	"time"
)

func TestPublishes(t *testing.T) {
	go publish()
	time.Sleep(10 * time.Millisecond)
	if !ready.Load() {
		t.Fatal("publication did not finish")
	}
}
