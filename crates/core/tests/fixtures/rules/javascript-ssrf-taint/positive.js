function handle(req) {
  const target = req.param("url");
  fetch(target);
}
