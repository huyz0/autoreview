function handle(req: any) {
  const target: string = req.param("url");
  fetch(target);
}
