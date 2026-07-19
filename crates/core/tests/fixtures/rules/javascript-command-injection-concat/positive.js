const { exec } = require("child_process");
function run(userInput) {
  exec("ls " + userInput);
}
