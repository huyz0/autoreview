class Sample {
    int totalLength(String[] items) {
        int total = 0;
        for (int i = 0; i <= items.length; i++) {
            total += items[i].length();
        }
        return total;
    }
}
