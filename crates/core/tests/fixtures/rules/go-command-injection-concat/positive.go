package main

func f() {
	cmd := exec.Command("sh", "-c", "echo "+userInput)
	_ = cmd
}
