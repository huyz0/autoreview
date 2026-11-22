package flow

func runBothDirections(n int) {
	for i := 0; i < n; i++ {
		println(i)
	}
	for j := n; j >= 0; j-- {
		println(j)
	}
}
