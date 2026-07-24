class Main {
    fun handle(req: HttpServletRequest, response: HttpServletResponse) {
        val target = req.getParameter("next")
        response.sendRedirect(target)
    }
}
