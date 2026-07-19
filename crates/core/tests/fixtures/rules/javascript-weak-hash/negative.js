const crypto = require("crypto");
function h(s) {
  return crypto.createHash("sha256").update(s).digest("hex");
}
