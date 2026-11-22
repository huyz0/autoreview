import java.util.Comparator;
import java.util.List;
import java.util.Optional;
import java.util.stream.Stream;

class ScoreRanking {
    Optional<Integer> minimum(List<Integer> scores) {
        return scores.stream().min(Comparator.naturalOrder());
    }

    List<Integer> topTwo(List<Integer> scores) {
        Stream<Integer> sorted = scores.stream().sorted();
        return sorted.limit(2).toList();
    }
}
