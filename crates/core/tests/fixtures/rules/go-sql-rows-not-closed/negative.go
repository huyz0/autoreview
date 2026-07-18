package main

import "database/sql"

func query(db *sql.DB) {
	rows, err := db.Query("SELECT 1")
	if err != nil {
		return
	}
	defer rows.Close()
	for rows.Next() {
	}
}
