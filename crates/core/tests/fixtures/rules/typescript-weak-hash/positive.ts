import * as crypto from "crypto";
function h(s: string) {
  return crypto.createHash("md5").update(s).digest("hex");
}
