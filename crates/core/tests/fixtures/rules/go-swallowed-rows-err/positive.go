package example

import "database/sql"

func consume(rows *sql.Rows) {
	for rows.Next() {
		var value string
		if err := rows.Scan(&value); err != nil {
			return
		}
	}
	_ = rows.Err()
}
