import * as crypto from "crypto";
function h(s: string) {
  return crypto.createHash("sha256").update(s).digest("hex");
}
