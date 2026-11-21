package example

import "time"

func setup() {
	time.Sleep(50 * time.Millisecond)
	time.Sleep(0)
	localClock{}.Sleep(50)
}

type localClock struct{}

func (localClock) Sleep(ticks int) {}
