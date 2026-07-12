package main

func f() {
	rows, err := db.Query("SELECT * FROM users WHERE id = " + userID)
	_ = rows
	_ = err
}
