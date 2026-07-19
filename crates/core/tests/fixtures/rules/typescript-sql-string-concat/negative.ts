function q(db: any, name: string) {
  db.query("SELECT * FROM users WHERE name = $1", [name]);
}
