import { execFile } from "child_process";
function run(userInput: string) {
  execFile("ls", [userInput]);
}
