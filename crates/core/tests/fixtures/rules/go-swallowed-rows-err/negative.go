package example

import "database/sql"

func collect(rows *sql.Rows) error {
	for rows.Next() {
		read(rows)
	}
	return rows.Err()
}

func call(rows *customRows) {
	_ = rows.Err()
}
