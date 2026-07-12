package main

import "net/http"

func f() {
	c := &http.Cookie{Name: "session", Secure: true}
	_ = c
}
