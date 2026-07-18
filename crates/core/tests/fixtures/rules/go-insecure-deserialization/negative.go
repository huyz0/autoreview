package foo

import "encoding/json"

func handle(body []byte) {
	var v MyType
	json.Unmarshal(body, &v)
}
