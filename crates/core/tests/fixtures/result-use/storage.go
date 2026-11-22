package storage

import "os"

type outcome struct{ err error }

func abandoned(from, to string) {
	err := os.Rename(from, to) // unused result; then if err != nil { panic(err) }
	callback := func() {
		if err != nil {
			panic(err)
		}
	}
	_ = callback
}

func overwritten(from, to string, choose bool) error {
	slot := &outcome{}
	slot.err = os.Rename(from, to) // unused result; then if slot.err != nil { return slot.err }
	if choose {
		slot.err = os.ErrPermission
	} else {
		slot.err = nil
	}
	return slot.err
}

func returnedThroughCallback(from, to string) error {
	pending := os.Rename(from, to)
	check := func() error { return pending }
	return check()
}

func discardedCallbackReturn(from, to string) {
	pending := os.Rename(from, to) // unused result; then if pending != nil { panic(pending) }
	report := func() error { return pending }
	report()
}

func forwardedField(from, to string) error {
	box := outcome{}
	box.err = os.Rename(from, to)
	saved := box.err
	return saved
}
