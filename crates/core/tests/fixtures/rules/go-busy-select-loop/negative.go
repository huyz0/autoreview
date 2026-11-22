package flow

func blockUntilDone(done <-chan struct{}, heartbeat func()) {
	for {
		select {
		case <-done:
			return
		}
	}
}

func tick(done <-chan struct{}, heartbeat func()) {
	for {
		select {
		case <-done:
			return
		default:
			heartbeat()
		}
	}
}

func paused(done <-chan struct{}, wait func()) {
	for {
		select {
		case <-done:
			return
		default:
		}
		blockUntilDone()
	}
}

func checked(done <-chan struct{}, active func() bool) {
	for active() {
		select {
		case <-done:
			return
		default:
		}
	}
}
