class Main {
    fun handle(req: HttpServletRequest, stmt: Statement) {
        val q = req.getParameter("q")
        stmt.executeQuery(q)
    }
}
