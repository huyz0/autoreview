use std::io;

fn update(program: &str) -> io::Result<()> {
    std::process::Command::new(program).arg("--refresh").status()?;
    Ok(())
}
