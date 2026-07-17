package main

func f(db *DB, ids []int) {
	for i := 0; i < len(ids); i++ {
		rows, err := db.Query("select * from t where id=?", ids[i])
		_ = rows
		_ = err
	}
}
