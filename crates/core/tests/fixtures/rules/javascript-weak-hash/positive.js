const crypto = require("crypto");
function h(s) {
  return crypto.createHash("md5").update(s).digest("hex");
}
