#[cfg(target_os = "macos")]
mod imp {
    use anyhow::{Context, Result};
    use std::path::Path;

    pub const OVERRIDE_STAT_XATTR: &str = "user.containers.override_stat";

    /// Stamp libkrun's `user.containers.override_stat` xattr on a regular file
    /// so the guest sees it owned by `uid:gid` with the given permission bits.
    /// Uses the 3-field form `"<uid>:<gid>:0<mode_octal>"` (S_IFMT bits stripped);
    /// libkrun ORs in the host's real type bits at read time.
    pub fn set_override_stat_file(path: &Path, uid: u32, gid: u32, mode: u32) -> Result<()> {
        let value = format!("{}:{}:0{:o}", uid, gid, mode & 0o7777);
        xattr::set(path, OVERRIDE_STAT_XATTR, value.as_bytes())
            .with_context(|| format!("setxattr {} on {}", OVERRIDE_STAT_XATTR, path.display()))
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use anyhow::Result;
    use std::path::Path;

    pub fn set_override_stat_file(_: &Path, _: u32, _: u32, _: u32) -> Result<()> {
        Ok(())
    }
}

pub use imp::set_override_stat_file;
