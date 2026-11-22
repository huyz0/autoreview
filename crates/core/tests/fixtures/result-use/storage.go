package storage

import "os"

type RenameOutcome struct{ err error }

func Abandoned(from, to string) {
	err := os.Rename(from, to) // unused result; then if err != nil { panic(err) }
	callback := func() {
		if err != nil {
			panic(err)
		}
	}
	_ = callback
}

func Overwritten(from, to string, choose bool) error {
	slot := &RenameOutcome{}
	slot.err = os.Rename(from, to) // unused result; then if slot.err != nil { return slot.err }
	if choose {
		slot.err = os.ErrPermission
	} else {
		slot.err = nil
	}
	return slot.err
}

func ReturnedThroughCallback(from, to string) error {
	pending := os.Rename(from, to)
	check := func() error { return pending }
	return check()
}

func DiscardedCallbackReturn(from, to string) {
	pending := os.Rename(from, to) // unused result; then if pending != nil { panic(pending) }
	report := func() error { return pending }
	_ = report()
}

func ForwardedField(from, to string) error {
	box := RenameOutcome{}
	box.err = os.Rename(from, to)
	saved := box.err
	return saved
}
