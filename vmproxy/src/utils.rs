use std::process::Command;

use anyhow::Context;

pub fn script(script: &str) -> Command {
    let mut cmd = Command::new("/bin/busybox");
    cmd.arg("sh").arg("-c").arg(script);
    cmd
}

pub fn script_output(code: &str) -> anyhow::Result<String> {
    Ok(String::from_utf8_lossy(
        &script(code)
            .output()
            .context("Failed to run script command")?
            .stdout,
    )
    .into())
}
