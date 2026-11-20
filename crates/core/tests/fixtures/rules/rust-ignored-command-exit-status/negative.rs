use std::io;

fn run(program: &str) -> io::Result<bool> {
    let _ = std::process::Command::new(program).arg("--refresh").status()?;
    let output = std::process::Command::new(program).output()?;
    let success = std::process::Command::new(program).status()?.success();
    Ok(success && output.status.success())
}

fn passthrough(command: std::process::Command) -> std::process::Command {
    command
}

fn unrelated_receiver(program: &str) -> io::Result<()> {
    passthrough(std::process::Command::new(program)).arg("--refresh").status()?;
    Ok(())
}
