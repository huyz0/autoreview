package main

func handle(r *http.Request, db *sql.DB) {
	id := r.FormValue("id")
	rows, err := db.Query(id)
	_ = rows
	_ = err
}
