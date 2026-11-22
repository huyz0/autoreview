package main

import "slices"

type query struct{}

func (*query) Find(_ *[]string) struct{ Error error } {
	return struct{ Error error }{}
}

func Run(db *query) {
	var users []string
	result := db.Find(&users)
	if result.Error != nil {
		return
	}
}

func DiscardStandardSliceResult(records []int) {
	slices.Delete(records, 0, 1)
}
