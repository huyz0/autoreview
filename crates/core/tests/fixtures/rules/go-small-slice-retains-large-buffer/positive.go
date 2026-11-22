package collections

func Prefix() []byte {
	buffer := make([]byte, 100000)
	return buffer[:32]
}
