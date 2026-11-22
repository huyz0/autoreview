class Sample {
    int sum(int n) {
        int total = 0;
        for (int i = n; i >= 0; --i) total += i;
        for (int j = 0; j < n; j++) total += j;
        return total;
    }
}
