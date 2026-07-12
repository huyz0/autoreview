package main

func f() {
	cmd := exec.Command("sh", "-c", "echo hello")
	_ = cmd
}
