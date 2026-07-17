package main

func f(db *DB, ids []int) {
	rows, err := db.Query("select * from t where id in (?)", ids)
	_ = rows
	_ = err
}
