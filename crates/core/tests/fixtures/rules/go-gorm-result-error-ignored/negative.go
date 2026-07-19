package main

func run(db *gorm.DB) {
	var users []User
	result := db.Find(&users)
	if result.Error != nil {
		return
	}
}
