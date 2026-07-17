class Sample {
    void f(Statement stmt, int[] ids) {
        for (int i = 0; i < ids.length; i++) {
            ResultSet rs = stmt.executeQuery("select * from t where id=" + ids[i]);
        }
    }
}
