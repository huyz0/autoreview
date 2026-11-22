package collections

import "slices"

func header() []byte {
	bytes := make([]byte, 200000)
	return slices.Clone(bytes[:16])
}

func smallSample() []byte {
	data := make([]byte, 256)
	return data[:16]
}
