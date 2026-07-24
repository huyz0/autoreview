class Main {
    void handle(String xml) throws Exception {
        DocumentBuilderFactory dbf = DocumentBuilderFactory.newInstance();
        dbf.setFeature("http://apache.org/xml/features/disallow-doctype-decl", true);
        DocumentBuilder db = dbf.newDocumentBuilder();
        db.parse(xml);
    }
}
