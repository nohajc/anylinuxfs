use anyhow::Context;
use bstr::{B, BStr, BString, ByteSlice};
use common_utils::{PathExt, host_eprintln, host_println};
use derive_more::{Deref, DerefMut};
use rayon::prelude::*;
use std::{
    collections::{BTreeMap, HashSet},
    ffi::{OsStr, OsString},
    fs, io,
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};

#[cfg(target_os = "macos")]
use common_utils::FromPath;
#[cfg(target_os = "macos")]
use std::{
    ffi::{CStr, CString},
    mem,
    ptr::null_mut,
};

#[derive(Debug, Clone)]
pub struct MountTable {
    disks: HashSet<OsString>,
    mount_points: HashSet<OsString>,
}

impl MountTable {
    #[cfg(target_os = "macos")]
    pub fn new() -> io::Result<Self> {
        let count = unsafe { libc::getfsstat(null_mut(), 0, libc::MNT_NOWAIT) };
        if count < 0 {
            return Err(io::Error::last_os_error());
        }

        let mounts_raw: Vec<libc::statfs> = vec![unsafe { std::mem::zeroed() }; count as usize];
        let res = unsafe {
            libc::getfsstat(
                mounts_raw.as_ptr() as *mut libc::statfs,
                mem::size_of_val(mounts_raw.as_slice()) as libc::c_int,
                libc::MNT_NOWAIT,
            )
        };
        if res < 0 {
            return Err(io::Error::last_os_error());
        }

        let mut disks = HashSet::new();
        let mut mount_points = HashSet::new();
        for buf in mounts_raw {
            let StatfsBuf {
                mntfromname,
                mntonname,
            } = buf.into();

            if !mntfromname.is_empty() && !mntonname.is_empty() {
                disks.insert(mntfromname);
                mount_points.insert(mntonname);
            }
        }
        Ok(MountTable {
            disks,
            mount_points,
        })
    }

    #[cfg(target_os = "linux")]
    pub fn new() -> io::Result<Self> {
        let mounts =
            procfs::mounts().map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;
        let mut disks = HashSet::new();
        let mut mount_points = HashSet::new();
        for entry in mounts {
            disks.insert(OsString::from(entry.fs_spec));
            mount_points.insert(OsString::from(entry.fs_file));
        }
        Ok(MountTable {
            disks,
            mount_points,
        })
    }

    pub fn is_mounted(&self, path: impl AsRef<Path>) -> bool {
        let path = path.as_ref();
        self.disks.contains(path.as_os_str())
    }

    pub fn mount_points(&self) -> impl Iterator<Item = &OsString> {
        self.mount_points.iter()
    }
}

/// NFS option key that disables file locking. macOS spells it `nolocks`,
/// Linux spells it `nolock` (no trailing `s`). Use this constant whenever
/// inserting/removing the option so the spelling difference doesn't leak
/// into call sites.
#[cfg(target_os = "macos")]
pub const NOLOCK_KEY: &str = "nolocks";
#[cfg(target_os = "linux")]
pub const NOLOCK_KEY: &str = "nolock";

#[derive(Debug, Clone, Deref, DerefMut)]
pub struct NfsOptions(BTreeMap<BString, BString>);

impl Default for NfsOptions {
    fn default() -> Self {
        let mut opts = BTreeMap::new();
        #[cfg(target_os = "macos")]
        {
            opts.insert("deadtimeout".into(), "45".into()); // this is what Finder uses
            opts.insert("nfc".into(), "".into()); // NFC Unicode normalization (macOS-only)

            // Soft mount semantics to bound kernel-level retries when the
            // underlying microVM becomes unreachable (e.g. user hot-unplugs
            // a managed USB drive without running `anylinuxfs unmount` first).
            //
            // Without this, macOS NFS client (default hard mount) retries
            // indefinitely against the dead NFS server, holds IOMediaBSDClient
            // busy, and triggers `panic(busy timeout[1])` once kernel watchdogd
            // notices a registry entry stuck for 60s.
            opts.insert("soft".into(), "".into());
            opts.insert("intr".into(), "".into());
            opts.insert("timeo".into(), "100".into()); // tenths of a second → 10s per try
            opts.insert("retrans".into(), "3".into());
        }
        opts.insert(NOLOCK_KEY.into(), "".into());
        opts.insert("vers".into(), "3".into());
        opts.insert("port".into(), "2049".into());
        opts.insert("mountport".into(), "32767".into());
        NfsOptions(opts)
    }
}

impl NfsOptions {
    pub fn to_list(&self) -> Vec<u8> {
        bstr::join(
            ",",
            self.0.iter().map(|(k, v)| {
                if v.is_empty() {
                    k.to_owned()
                } else {
                    bstr::join("=", [k, v]).into()
                }
            }),
        )
    }
}

pub fn mounted_from(path: impl AsRef<Path>) -> io::Result<PathBuf> {
    let buf = statfs(path.as_ref())?;

    if path.as_ref() != buf.mntonname.as_os_str() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("Path '{}' is not a mount point.", path.as_ref().display(),),
        ));
    }

    Ok(buf.mntfromname.into())
}

#[cfg(target_os = "macos")]
fn os_str_from_c_chars(chars: &[i8]) -> &OsStr {
    let cstr = unsafe { CStr::from_ptr(chars.as_ptr()) };
    OsStr::from_bytes(cstr.to_bytes())
}

struct StatfsBuf {
    mntfromname: OsString,
    mntonname: OsString,
}

#[cfg(target_os = "macos")]
impl From<libc::statfs> for StatfsBuf {
    fn from(buf: libc::statfs) -> Self {
        StatfsBuf {
            mntfromname: os_str_from_c_chars(&buf.f_mntfromname).to_owned(),
            mntonname: os_str_from_c_chars(&buf.f_mntonname).to_owned(),
        }
    }
}

#[cfg(target_os = "macos")]
fn statfs(path: impl AsRef<Path>) -> io::Result<StatfsBuf> {
    let c_path = CString::from_path(path.as_ref());
    let mut buf: libc::statfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statfs(c_path.as_ptr(), &mut buf) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(buf.into())
}

#[cfg(target_os = "linux")]
fn statfs(path: impl AsRef<Path>) -> io::Result<StatfsBuf> {
    let path_str = path.as_ref().to_string_lossy().into_owned();
    let mounts =
        procfs::mounts().map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;
    for entry in mounts {
        if entry.fs_file == path_str {
            return Ok(StatfsBuf {
                mntfromname: OsString::from(entry.fs_spec),
                mntonname: OsString::from(entry.fs_file),
            });
        }
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        format!("'{}' not found in /proc/mounts", path_str),
    ))
}

/// Best-effort: if /proc/mounts has an entry for `device`, force-umount it.
/// Used on the failure path to avoid leaving a client-side NFS mount that
/// points at a VM we're about to tear down. Linux's default `hard` NFS client
/// would hang indefinitely on server death, freezing any shell that later
/// stats the mount point. macOS relies on DiskArbitration teardown — no-op.
#[cfg(target_os = "linux")]
pub fn cleanup_stale_nfs_client_mount(device: &str) -> anyhow::Result<()> {
    let mounts = procfs::mounts().context("Failed to read /proc/mounts")?;
    for entry in mounts {
        if entry.fs_spec == device {
            let _ = Command::new("umount")
                .arg("-f")
                .arg(&entry.fs_file)
                .status();
        }
    }
    Ok(())
}

#[cfg(target_os = "macos")]
pub fn cleanup_stale_nfs_client_mount(_device: &str) -> anyhow::Result<()> {
    Ok(())
}

// If `copied` refers to a file which should be synchronized with `orig`,
// we can detect whether the copied file is too old by comparing mtime.
// Also, if file sizes differ, we trivially know the files are different.
pub fn files_likely_differ(
    orig: impl AsRef<Path>,
    copied: impl AsRef<Path>,
) -> anyhow::Result<bool> {
    let orig = orig.as_ref();
    let copied = copied.as_ref();
    let orig_md = fs::metadata(&orig).context(format!("Error accessing {}", orig.display()))?;
    let copied_md =
        fs::metadata(&copied).context(format!("Error accessing {}", copied.display()))?;

    if orig_md.len() != copied_md.len() {
        return Ok(true);
    }

    if orig_md.modified()? > copied_md.modified()? {
        return Ok(true);
    }

    Ok(false)
}

mod dirtrie {
    use std::{collections::BTreeMap, ffi::OsString, fmt::Display, path::Path};

    use bstr::{BStr, BString};

    #[derive(Debug, Default)]
    pub struct Node {
        pub paths: Option<(OsString, BString)>,
        pub children: BTreeMap<OsString, Node>,
    }

    impl Node {
        pub fn insert(&mut self, path: &Path, full_path: &BStr) {
            let mut current = self;
            for segment in path.components() {
                let segment = segment.as_os_str().to_owned();
                current = current.children.entry(segment).or_default();
            }
            current.paths = Some((path.as_os_str().into(), full_path.into()));
        }
    }

    impl Display for Node {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            fn fmt_node(
                node: &Node,
                f: &mut std::fmt::Formatter<'_>,
                prefix: &str,
            ) -> std::fmt::Result {
                for (segment, child) in &node.children {
                    write!(
                        f,
                        "{}{} ({})\r\n",
                        prefix,
                        segment.to_string_lossy(),
                        child
                            .paths
                            .as_ref()
                            .map(|(_, p)| p.clone())
                            .unwrap_or(b"".into())
                    )?;
                    fmt_node(child, f, &format!("{}--", prefix))?;
                }
                Ok(())
            }
            fmt_node(self, f, "")
        }
    }
}

fn parallel_mount_recursive(
    vm_host: &[u8],
    mnt_point_base: PathBuf,
    trie: &dirtrie::Node,
    nfs_opts: &BStr,
    elevate: bool,
) -> anyhow::Result<()> {
    if let Some((rel_path, nfs_path)) = &trie.paths {
        let shell_script = [
            b"mount -t nfs -o ",
            nfs_opts.as_bytes(),
            b" \"",
            vm_host,
            b":",
            nfs_path.as_bytes(),
            b"\" \"",
            mnt_point_base.join(rel_path).as_bytes(),
            b"\"",
        ]
        .concat();
        // host_println!("Running NFS mount command: `{}`", &shell_script);

        // elevate if needed (e.g. mounting image under /Volumes)
        let cmdline = [B("sudo"), B("-S"), B("sh"), B("-c"), &shell_script].map(OsStr::from_bytes);
        let cmdline = if elevate { &cmdline[..] } else { &cmdline[2..] };
        let status = Command::new(cmdline[0]).args(&cmdline[1..]).status()?;

        if !status.success() {
            anyhow::bail!(
                "mount failed with exit code {}",
                status
                    .code()
                    .map(|c| c.to_string())
                    .unwrap_or("unknown".to_owned())
            );
        }
        host_println!(
            "Mounted subdirectory: {}",
            mnt_point_base.join(rel_path).display()
        );
    }
    trie.children.par_iter().try_for_each(|(_, child)| {
        parallel_mount_recursive(vm_host, mnt_point_base.clone(), child, nfs_opts, elevate)
    })?;

    Ok(())
}

pub fn mount_nfs_subdirs<'a>(
    vm_host: &[u8],
    share_path_base: &[u8],
    subdirs: impl Iterator<Item = &'a str>,
    mnt_point_base: impl AsRef<Path>,
    nfs_opts: &NfsOptions,
    elevate: bool,
) -> anyhow::Result<()> {
    let mut trie = dirtrie::Node::default();

    for subdir in subdirs.map(BStr::new) {
        let subdir_relative = subdir
            .strip_prefix(share_path_base)
            .and_then(|s| s.strip_prefix(b"/"))
            .unwrap_or(b"");

        trie.insert(Path::from_bytes(subdir_relative), subdir.into());
    }

    parallel_mount_recursive(
        vm_host,
        mnt_point_base.as_ref().into(),
        &trie,
        nfs_opts.to_list().as_bstr(),
        elevate,
    )?;
    // host_println!("Mounted NFS subdirectories:\r\n{}", trie);
    Ok(())
}

fn parallel_unmount_recursive(trie: &dirtrie::Node) -> anyhow::Result<()> {
    trie.children
        .par_iter()
        .try_for_each(|(_, child)| parallel_unmount_recursive(child))?;

    if let Some((_, mount_path)) = &trie.paths {
        #[cfg(target_os = "macos")]
        let shell_script = format!("diskutil unmount \"{}\"", mount_path);
        #[cfg(target_os = "linux")]
        let shell_script = format!("umount \"{}\"", mount_path);
        // exit status ignored, we don't want to exit early if one unmount fails
        let _ = Command::new("sh").arg("-c").arg(&shell_script).status()?;
    }
    Ok(())
}

pub fn unmount_nfs_subdirs<'a>(
    subdirs: impl Iterator<Item = &'a OsStr>,
    mnt_point_base: impl AsRef<Path>,
) -> anyhow::Result<()> {
    let mut trie = dirtrie::Node::default();
    let base = mnt_point_base.as_ref().as_bytes();

    for subdir in subdirs {
        let subdir_bytes = subdir.as_bytes();
        // Compute the path relative to mnt_point_base. Two valid shapes:
        //   - subdir == base                                     -> ""
        //   - subdir == base + "/" + rest                        -> "rest"
        // Anything else is a caller bug — distinct mount points can't share
        // a trie key without one silently overwriting the other (the trie
        // root collides on key=""). Skip with a warning rather than risk a
        // dropped umount.
        let subdir_relative: &[u8] = if subdir_bytes == base {
            b""
        } else if let Some(rest) = subdir_bytes
            .strip_prefix(base)
            .and_then(|s| s.strip_prefix(b"/"))
        {
            rest
        } else {
            host_eprintln!(
                "warning: unmount_nfs_subdirs: skipping {:?} — not under base {:?}",
                <[u8]>::as_bstr(subdir_bytes),
                <[u8]>::as_bstr(base),
            );
            continue;
        };

        trie.insert(&*subdir_relative.to_path_lossy(), subdir_bytes.into());
    }

    parallel_unmount_recursive(&trie)?;
    Ok(())
}

pub fn wait_for_file(file: impl AsRef<Path>) -> anyhow::Result<()> {
    let start = Instant::now();
    while !file.as_ref().exists() {
        if start.elapsed() > Duration::from_secs(5) {
            anyhow::bail!(
                "Timeout waiting for file creation: {}",
                file.as_ref().display()
            );
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Ok(())
}

#[cfg(test)]
#[cfg(target_os = "macos")]
mod tests {
    use super::*;

    #[test]
    fn default_nfs_opts() {
        let opts = NfsOptions::default();
        let opts_str = String::from_utf8(opts.to_list())
            .expect("NfsOptions::to_list() should produce valid UTF-8 for ASCII keys");

        assert!(
            opts_str.contains("soft"),
            "missing 'soft' option in default macOS NFS opts: {opts_str}"
        );
        assert!(
            opts_str.contains("intr"),
            "missing 'intr' option in default macOS NFS opts: {opts_str}"
        );
        assert!(
            opts_str.contains("timeo=100"),
            "missing 'timeo=100' option (per-retry timeout): {opts_str}"
        );
        assert!(
            opts_str.contains("retrans=3"),
            "missing 'retrans=3' option (max retransmissions): {opts_str}"
        );

        assert!(
            opts_str.contains("deadtimeout=45"),
            "existing 'deadtimeout=45' removed: {opts_str}"
        );
        assert!(
            opts_str.contains("nfc"),
            "existing 'nfc' option removed: {opts_str}"
        );
        assert!(
            opts_str.contains("vers=3"),
            "existing 'vers=3' removed: {opts_str}"
        );
        assert!(
            opts_str.contains("port=2049"),
            "existing 'port=2049' removed: {opts_str}"
        );
        assert!(
            opts_str.contains("mountport=32767"),
            "existing 'mountport=32767' removed: {opts_str}"
        );
    }
}
