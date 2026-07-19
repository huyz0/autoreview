package main

func run(db *gorm.DB) {
	var users []User
	db.Find(&users)
}
