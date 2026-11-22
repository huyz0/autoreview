package collections

func prefix() []byte {
	buffer := make([]byte, 100000)
	return buffer[:32]
}
