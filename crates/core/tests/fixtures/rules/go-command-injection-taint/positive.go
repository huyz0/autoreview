package main

func handle(r *http.Request) {
	userInput := r.FormValue("cmd")
	exec.Command("sh", "-c", userInput)
}
