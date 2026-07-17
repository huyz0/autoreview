class Sample {
    void f(Statement stmt) {
        ResultSet rs = stmt.executeQuery("select * from t");
    }
}
