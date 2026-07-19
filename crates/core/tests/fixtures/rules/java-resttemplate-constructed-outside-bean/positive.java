public class Sample {
    public Widget fetch(String id) {
        RestTemplate rt = new RestTemplate();
        return rt.getForObject("/widgets/" + id, Widget.class);
    }
}
