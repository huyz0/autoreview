class Main {
    fun handle(xml: String) {
        val dbf = DocumentBuilderFactory.newInstance()
        val db = dbf.newDocumentBuilder()
        db.parse(xml)
    }
}
