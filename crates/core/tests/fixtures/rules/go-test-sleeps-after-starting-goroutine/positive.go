package example

import (
	"sync/atomic"
	"testing"
	"time"
)

func TestPublishes(t *testing.T) {
	var ready atomic.Bool
	go ready.Store(true)
	time.Sleep(10 * time.Millisecond)
	if !ready.Load() {
		t.Fatal("publication did not finish")
	}
}
