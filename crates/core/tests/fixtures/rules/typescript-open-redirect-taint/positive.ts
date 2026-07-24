function handle(req: any, res: any) {
  const target: string = req.param("next");
  res.redirect(target);
}
