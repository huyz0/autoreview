class Main {
    fun handle(xml: String) {
        val dbf = DocumentBuilderFactory.newInstance()
        dbf.setFeature("http://apache.org/xml/features/disallow-doctype-decl", true)
        val db = dbf.newDocumentBuilder()
        db.parse(xml)
    }
}
