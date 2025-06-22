use std::{
    env, fs,
    mem::{self, MaybeUninit},
    process::Command,
    thread,
    time::Duration,
};

use anyhow::Context;
use libc::{c_int, off_t};

fn main() -> anyhow::Result<()> {
    let args = env::args().collect::<Vec<_>>();
    println!("Arguments: {:?}", args);
    let shm = ipio::Shm::create_anonymous(4096)?;

    let exec_path = fs::canonicalize(env::current_exe().context("Failed to get executable path")?)
        .context("Failed to get resolved exec path")?;
    println!("Executable path: {:?}", exec_path);

    if args.len() != 3 {
        let mut cmd = Command::new(exec_path)
            .args(&[format!("{}", shm.raw_fd()), format!("{}", shm.size())])
            .spawn()
            .context("Failed to spawn child process")?;

        // Parent process
        thread::sleep(Duration::from_secs(1));
        let data = unsafe { shm.data() };
        let data: &[u8] = unsafe { mem::transmute(&data[..4]) };
        println!("Parent process read data: {:#x?}", data);

        cmd.wait().context("Failed to wait for child process")?;
    } else {
        // Child process
        let shm_fd = args[1].parse::<c_int>().context("Failed to parse shm_fd")?;
        let shm_size = args[2]
            .parse::<off_t>()
            .context("Failed to parse shm_size")?;
        let shm = ipio::Shm::from_fd(shm_fd, shm_size)?;

        let data = unsafe { shm.data() };
        data[0] = MaybeUninit::new(0xca);
        data[1] = MaybeUninit::new(0xfe);
        data[2] = MaybeUninit::new(0xba);
        data[3] = MaybeUninit::new(0xbe);
    }

    Ok(())
}
