package main

func handle(w http.ResponseWriter, r *http.Request) {
	http.Redirect(w, r, "/home", 302)
}
