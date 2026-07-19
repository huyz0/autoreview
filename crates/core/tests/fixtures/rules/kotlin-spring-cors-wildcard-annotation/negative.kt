@RestController
@CrossOrigin(origins = ["https://example.com"])
class Sample {
    @GetMapping("/widgets")
    fun list(): List<Widget> = emptyList()
}
