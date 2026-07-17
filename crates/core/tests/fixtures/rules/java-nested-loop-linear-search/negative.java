class Sample {
    void f(A[] as, Map<Integer,B> index) {
        for (int i = 0; i < as.length; i++) {
            index.get(as[i].id);
        }
    }
}
