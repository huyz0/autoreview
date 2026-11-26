package example

import "log"

func operation() error { return nil }
func propagated() error { if err := operation(); err != nil { return err }; return nil }
func logged() error { if err := operation(); err != nil { log.Print(err); return nil }; return nil }
func pointer() *int { if err := operation(); err != nil { return nil }; return nil }
func outer() error { f := func() *int { if err := operation(); err != nil { return nil }; return nil }; _ = f; return nil }
func otherName() error { if value := operation(); value != nil { return nil }; return nil }
func notCall(err error) error { if err := err; err != nil { return nil }; return nil }
