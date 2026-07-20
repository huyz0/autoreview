class Main {
    void handle(HttpServletRequest req, Statement stmt) {
        String q = req.getParameter("q");
        stmt.executeQuery(q);
    }
}
