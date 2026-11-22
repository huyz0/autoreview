import java.util.Comparator;
import java.util.List;
import java.util.Optional;

class ScoreRanking {
    Optional<Integer> minimum(List<Integer> scores) {
        return scores.stream().min(Comparator.naturalOrder());
    }

    List<Integer> topTwo(List<Integer> scores) {
        return scores.stream().sorted().limit(2).toList();
    }
}
