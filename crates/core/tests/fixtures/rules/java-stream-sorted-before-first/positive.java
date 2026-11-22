import java.util.List;
import java.util.Optional;

class FirstScore {
    Optional<Integer> lowest(List<Integer> scores) {
        return scores.stream().sorted().findFirst();
    }
}
