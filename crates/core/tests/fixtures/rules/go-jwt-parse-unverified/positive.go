package main

func f() {
	tok, parts, err := jwt.ParseUnverified(tokenString, claims)
	_ = tok
	_ = parts
	_ = err
}
