package foo

type Reader interface {
	Read(p []byte) (int, error)
}
