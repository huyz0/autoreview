@RestController
@CrossOrigin(origins = "*")
public class Sample {
    @GetMapping("/widgets")
    public List<Widget> list() {
        return null;
    }
}
