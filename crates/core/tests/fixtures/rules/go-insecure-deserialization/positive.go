package foo

import "encoding/gob"

func handle(body []byte) {
	var v MyType
	gob.NewDecoder(body).Decode(&v)
}
