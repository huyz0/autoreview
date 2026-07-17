class Sample {
    String f(String[] items) {
        StringBuilder b = new StringBuilder();
        for (int i = 0; i < items.length; i++) {
            b.append(items[i]);
        }
        return b.toString();
    }
}
