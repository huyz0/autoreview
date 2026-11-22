package flow

func countDown(x int) int {
	if x <= 0 {
		return x
	}
	return countDown(x - 1)
}
func outer()                  { inner() }
func inner()                  {}
func invoke(invoke func(any)) { invoke(invoke) }
