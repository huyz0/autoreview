function handle(req, db) {
  const q = req.param("q");
  db.query(q);
}
