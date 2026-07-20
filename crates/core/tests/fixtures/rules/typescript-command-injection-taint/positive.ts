function handle(req: any, cp: any) {
  const cmd: string = req.param("cmd");
  cp.exec(cmd);
}
