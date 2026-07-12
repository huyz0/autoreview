package main

func f() {
	tok, err := jwt.Parse(tokenString, keyFunc)
	_ = tok
	_ = err
}
