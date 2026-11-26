package example

func operation() error { return nil }

func run() error {
    if err := operation(); err != nil {
        return nil
    }
    return nil
}
