import { exec } from "child_process";
function run(userInput: string) {
  exec("ls " + userInput);
}
