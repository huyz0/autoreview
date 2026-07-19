package main

func f(db *sql.DB) {
	rows, err := db.Query("SELECT * FROM users")
	_ = rows
	_ = err
}
