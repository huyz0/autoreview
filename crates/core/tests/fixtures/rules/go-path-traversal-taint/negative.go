package main

func f() {
	data, err := os.ReadFile("/etc/config.json")
	_ = data
	_ = err
}
