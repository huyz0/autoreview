package main

func handle(r *http.Request) {
	name := r.FormValue("name")
	log.Printf("login attempt for %s", name)
}
