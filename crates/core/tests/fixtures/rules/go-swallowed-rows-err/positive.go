package example

import "database/sql"

func consume(rows *sql.Rows) {
	for rows.Next() {
		read(rows)
	}
	_ = rows.Err()
}
