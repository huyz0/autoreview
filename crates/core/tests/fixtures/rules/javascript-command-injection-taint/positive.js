function handle(req, cp) {
  const cmd = req.param("cmd");
  cp.exec(cmd);
}
