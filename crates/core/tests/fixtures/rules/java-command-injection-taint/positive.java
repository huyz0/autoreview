class Main {
    void handle(HttpServletRequest req) {
        String cmd = req.getParameter("cmd");
        Runtime.getRuntime().exec(cmd);
    }
}
