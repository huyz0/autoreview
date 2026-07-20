function handle(req: any, db: any) {
  const q: string = req.param("q");
  db.query(q);
}
