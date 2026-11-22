package collections

import "slices"

func Header() []byte {
	bytes := make([]byte, 200000)
	return slices.Clone(bytes[:16])
}

func SmallSample() []byte {
	data := make([]byte, 256)
	return data[:16]
}
