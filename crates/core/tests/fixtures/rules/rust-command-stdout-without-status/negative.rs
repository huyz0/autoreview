use std::{io, process::Command};

fn lines(program: &str) -> io::Result<Vec<u8>> {
    let _ = std::process::Command::new(program).output()?.stdout;
    let output = std::process::Command::new(program).args(["--list"]).output()?;
    if output.status.success() {
        Ok(output.stdout)
    } else {
        Err(io::Error::other("list failed"))
    }
}

fn from_existing(command: &mut Command) -> io::Result<Vec<u8>> {
    Ok(command.output()?.stdout)
}
