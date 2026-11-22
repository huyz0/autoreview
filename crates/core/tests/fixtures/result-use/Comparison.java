import java.util.Objects;
import java.util.function.BooleanSupplier;

class Comparison {
    static final class Slot {
        boolean value;
    }

    void abandoned(String left, String right) {
        boolean match = Objects.equals(left, right); // unused result; then System.out.println(match);
        Runnable printMatch = () -> System.out.println(match);
    }

    boolean wrongReceiver(Object left, Object right) {
        Slot checked = new Slot();
        Slot other = new Slot();
        checked.value = Objects.deepEquals(left, right); // unused result; then if (checked.value) return true;
        return other.value;
    }

    boolean throughSupplier(String stored, String input) {
        boolean equal = Objects.equals(stored, input);
        BooleanSupplier compare = () -> equal;
        return compare.getAsBoolean();
    }

    boolean throughField(Object a, Object b) {
        Slot holder = new Slot();
        holder.value = Objects.equals(a, b);
        return holder.value;
    }
}
