class Main {
    void handle(HttpServletRequest req, HttpServletResponse response) throws Exception {
        String target = req.getParameter("next");
        response.sendRedirect(target);
    }
}
