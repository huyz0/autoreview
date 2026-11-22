package flow

func wait(done <-chan struct{}) {
	for {
		select {
		case <-done:
			return
		default:
		}
	}
}
