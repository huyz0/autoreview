class Sample {
    String f(String[] items) {
        String s = "";
        for (int i = 0; i < items.length; i++) {
            s = s + items[i];
        }
        return s;
    }
}
