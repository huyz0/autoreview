function handle(req, res) {
  const target = req.param("next");
  res.redirect(target);
}
