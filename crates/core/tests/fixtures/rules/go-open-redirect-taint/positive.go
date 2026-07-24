package main

func handle(w http.ResponseWriter, r *http.Request) {
	target := r.FormValue("next")
	http.Redirect(w, r, target, 302)
}
