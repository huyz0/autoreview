package main

func handler(c *gin.Context) {
	var req Req
	c.ShouldBindJSON(&req)
}
