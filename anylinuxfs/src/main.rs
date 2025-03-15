use anyhow::Context;
use std::{env, ffi::CString, io, os::unix::ffi::OsStrExt, path::Path};

#[allow(unused)]
mod bindings;

fn main() {
    if let Err(e) = run() {
        eprintln!("Error: {}", e);
        std::process::exit(1);
    }
}

fn run() -> anyhow::Result<()> {
    let args: Vec<String> = env::args().collect();
    let disk_path = if args.len() > 1 {
        args[1].as_str()
    } else {
        eprintln!("No disk path provided");
        std::process::exit(1);
    };
    let read_only = true; // TODO: make this configurable
    let root_path = env::current_exe()
        .context("Failed to get current executable path")?
        .parent()
        .context("Failed to get executable directory")?
        .join("vmroot");

    println!("disk_path: {}", disk_path);
    println!("root_path: {}", root_path.to_string_lossy());

    let ctx = unsafe { bindings::krun_create_ctx() }.context("Failed to create context")?;

    unsafe { bindings::krun_set_log_level(3) }.context("Failed to set log level")?;

    unsafe { bindings::krun_set_vm_config(ctx, 1, 512) }.context("Failed to set VM config")?;

    unsafe { bindings::krun_set_root(ctx, CString::from_path(root_path).as_ptr()) }
        .context("Failed to set root")?;

    unsafe {
        bindings::krun_add_disk(
            ctx,
            CString::new("data").unwrap().as_ptr(),
            CString::new(disk_path).unwrap().as_ptr(),
            read_only,
        )
    }
    .context("Failed to add disk")?;

    unsafe { bindings::krun_set_workdir(ctx, CString::new("/").unwrap().as_ptr()) }
        .context("Failed to set workdir")?;

    let args = vec![CString::new("/vmproxy").unwrap()];
    let argv = args
        .iter()
        .map(|s| s.as_ptr())
        .chain([std::ptr::null()])
        .collect::<Vec<_>>();
    let envp = vec![std::ptr::null()];

    unsafe { bindings::krun_set_exec(ctx, argv[0], argv.as_ptr(), envp.as_ptr()) }
        .context("Failed to set exec")?;

    unsafe { bindings::krun_start_enter(ctx) }.context("Failed to start VM")?;

    Ok(())
}

trait FromPath {
    fn from_path(path: impl AsRef<Path>) -> Self;
}

impl FromPath for CString {
    fn from_path(path: impl AsRef<Path>) -> Self {
        CString::new(path.as_ref().as_os_str().as_bytes()).unwrap()
    }
}

trait ResultWithCtx {
    type Value;
    fn context(self, msg: &str) -> anyhow::Result<Self::Value>;
}

impl ResultWithCtx for i32 {
    type Value = u32;
    fn context(self, msg: &str) -> anyhow::Result<Self::Value> {
        if self < 0 {
            Err(io::Error::last_os_error()).context(msg.to_owned())
        } else {
            Ok(self as u32)
        }
    }
}
