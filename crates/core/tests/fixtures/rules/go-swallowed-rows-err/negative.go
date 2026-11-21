package example

import "database/sql"

func collect(rows *sql.Rows) error {
	for rows.Next() {
		var value string
		if err := rows.Scan(&value); err != nil {
			return err
		}
	}
	return rows.Err()
}

func call(rows *customRows) {
	_ = rows.Err()
}

type customRows struct{}

func (*customRows) Err() error { return nil }
