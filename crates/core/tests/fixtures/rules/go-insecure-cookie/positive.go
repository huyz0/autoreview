package main

import "net/http"

func f() {
	c := &http.Cookie{Name: "session", Secure: false}
	_ = c
}
