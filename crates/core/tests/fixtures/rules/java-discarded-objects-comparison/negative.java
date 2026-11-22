class Metadata {
    static String format(String[] actual, String[] expected) {
        var matches = java.util.Objects.deepEquals(actual, expected);
        return matches ? "unchanged" : "updated";
    }
}
