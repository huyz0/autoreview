@RestController
@CrossOrigin(origins = "https://example.com")
public class Sample {
    @GetMapping("/widgets")
    public List<Widget> list() {
        return null;
    }
}
