import java.util.Objects;

class Label {
    boolean matches(Object current, Object expected, Object backup) {
        boolean matches = Objects.equals(current, expected);
        matches = current == backup;
        return matches;
    }
}
