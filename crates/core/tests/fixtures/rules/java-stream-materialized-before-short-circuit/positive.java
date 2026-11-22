import java.util.List;
import java.util.Optional;

class CatalogLookup {
    Optional<String> firstEligible(List<String> names) {
        return names.stream().filter(name -> name.startsWith("a")).toList().stream().findFirst();
    }
}
