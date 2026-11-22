class Sample { void run(java.util.List<String> items, java.util.Set<String> allowed) { for (String item : items) { if (allowed.contains(item)) consume(item); } } void consume(String item) {} }
