function q(db, name) {
  db.query("SELECT * FROM users WHERE name = $1", [name]);
}
