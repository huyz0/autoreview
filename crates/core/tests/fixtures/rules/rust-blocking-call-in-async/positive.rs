async fn read_data() -> std::io::Result<String> {
    std::fs::read_to_string("data.txt")
}
