use std::fs;

use anyhow::Context;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 3 {
        eprintln!("Usage: {} <service_name> <file_name>", args[0]);
        std::process::exit(1);
    }
    let service_name = &args[1];
    let file_name = &args[2];
    // println!("Service name: {}", service_name);
    // println!("File name: {}", file_name);

    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(file_name)
        .context("Failed to open file")?;

    ipio::start_io_server(service_name, file).context("Failed to start IO server")?;
    Ok(())
}
