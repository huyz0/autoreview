@RestController
@CrossOrigin(origins = ["*"])
class Sample {
    @GetMapping("/widgets")
    fun list(): List<Widget> = emptyList()
}
