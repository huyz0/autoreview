package main

import "testing"

func TestGood(t *testing.T) {
	if got := doSomething(); got != 5 {
		t.Errorf("got %d", got)
	}
}
