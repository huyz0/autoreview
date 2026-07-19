const { execFile } = require("child_process");
function run(userInput) {
  execFile("ls", [userInput]);
}
