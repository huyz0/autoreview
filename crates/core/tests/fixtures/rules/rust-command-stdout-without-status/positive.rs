use std::io;

fn lines(program: &str) -> io::Result<Vec<u8>> {
    Ok(std::process::Command::new(program).args(["--list"]).output()?.stdout)
}
