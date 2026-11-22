import java.util.List;

class CatalogLookup {
    boolean anyEligible(List<String> names) {
        return names.stream().anyMatch(name -> name.startsWith("a"));
    }

    List<String> snapshot(List<String> names) {
        return names.stream().toList();
    }
}
