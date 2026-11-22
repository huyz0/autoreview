package flow

func run(x int) int {
	if x <= 0 {
		return x
	}
	return run(x - 1)
}
func outer()                  { inner() }
func inner()                  {}
func invoke(invoke func(any)) { invoke(invoke) }
