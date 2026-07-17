package main
func f(as []A, index map[int]B) {
	for i := 0; i < len(as); i++ {
		_ = index[as[i].ID]
	}
}
