import java.util.List;

class CatalogLookup {
    boolean anyEligible(List<String> names) {
        return names.stream().filter(name -> name.startsWith("a")).anyMatch(name -> name.length() >= 3);
    }

    List<String> snapshot(List<String> names) {
        return names.stream().filter(name -> name.startsWith("a")).toList();
    }
}
