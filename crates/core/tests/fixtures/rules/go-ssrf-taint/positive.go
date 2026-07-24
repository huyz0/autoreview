package main

func handle(r *http.Request) {
	target := r.FormValue("url")
	http.Get(target)
}
