package example

import "time"

func setup() {
	time.Sleep(50 * time.Millisecond)
	time.Sleep(0)
	length := 123
	timers.Sleep(length)
}
