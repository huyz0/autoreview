package main

func handle(r *http.Request) {
	name := r.FormValue("file")
	f, err := os.Open(name)
	_ = f
	_ = err
}
