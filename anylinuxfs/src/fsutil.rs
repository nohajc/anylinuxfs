use anyhow::{Context, anyhow};
use bstr::{BStr, BString, ByteSlice};
use common_utils::{FromPath, PathExt, host_println};
use derive_more::{Deref, DerefMut};
use rayon::prelude::*;
use std::{
    collections::{BTreeMap, HashSet},
    ffi::{CStr, CString, OsStr, OsString},
    fs, io, mem,
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
    process::Command,
    ptr::null_mut,
    time::{Duration, Instant},
};

#[derive(Debug, Clone)]
pub struct MountTable {
    disks: HashSet<OsString>,
    mount_points: HashSet<OsString>,
}

impl MountTable {
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
            let mntfromname = os_str_from_c_chars(&buf.f_mntfromname).to_owned();
            let mntonname = os_str_from_c_chars(&buf.f_mntonname).to_owned();
            // println!("mntfromname: {:?}", mntfromname);
            // println!("mntonname: {:?}", mntonname);

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

    pub fn is_mounted(&self, path: impl AsRef<Path>) -> bool {
        let path = path.as_ref();
        self.disks.contains(path.as_os_str())
    }

    pub fn mount_points(&self) -> impl Iterator<Item = &OsString> {
        self.mount_points.iter()
    }
}

#[derive(Debug, Clone, Deref, DerefMut)]
pub struct NfsOptions(BTreeMap<BString, BString>);

impl Default for NfsOptions {
    fn default() -> Self {
        let mut opts = BTreeMap::new();
        opts.insert("nfc".into(), "".into());
        opts.insert("vers".into(), "3".into());
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
    let mntfromname = os_str_from_c_chars(&buf.f_mntfromname);
    let mntonname = os_str_from_c_chars(&buf.f_mntonname);
    // println!("mntfromname: {:?}", mntfromname);
    // println!("mntonname: {:?}", mntonname);

    if path.as_ref() != mntonname {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("Path '{}' is not a mount point.", path.as_ref().display(),),
        ));
    }

    Ok(mntfromname.into())
}

fn statfs(path: impl AsRef<Path>) -> io::Result<libc::statfs> {
    let c_path = CString::from_path(path.as_ref());
    let mut buf: libc::statfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statfs(c_path.as_ptr(), &mut buf) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(buf)
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

fn os_str_from_c_chars(chars: &[i8]) -> &OsStr {
    let cstr = unsafe { CStr::from_ptr(chars.as_ptr()) };
    OsStr::from_bytes(cstr.to_bytes())
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
    mnt_point_base: PathBuf,
    trie: &dirtrie::Node,
    elevate: bool,
) -> anyhow::Result<()> {
    if let Some((rel_path, nfs_path)) = &trie.paths {
        let shell_script = format!(
            "mount -t nfs \"localhost:{}\" \"{}\"",
            nfs_path,
            mnt_point_base.join(rel_path).display()
        );
        // host_println!("Running NFS mount command: `{}`", &shell_script);

        // elevate if needed (e.g. mounting image under /Volumes)
        let cmdline = ["sudo", "-S", "sh", "-c", &shell_script];
        let cmdline = if elevate { &cmdline[..] } else { &cmdline[2..] };
        let status = Command::new(cmdline[0]).args(&cmdline[1..]).status()?;

        if !status.success() {
            return Err(anyhow!(
                "mount failed with exit code {}",
                status
                    .code()
                    .map(|c| c.to_string())
                    .unwrap_or("unknown".to_owned())
            ));
        }
        host_println!(
            "Mounted subdirectory: {}",
            mnt_point_base.join(rel_path).display()
        );
    }
    trie.children.par_iter().try_for_each(|(_, child)| {
        parallel_mount_recursive(mnt_point_base.clone(), child, elevate)
    })?;

    Ok(())
}

pub fn mount_nfs_subdirs<'a>(
    share_path_base: &[u8],
    subdirs: impl Iterator<Item = &'a str>,
    mnt_point_base: impl AsRef<Path>,
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

    parallel_mount_recursive(mnt_point_base.as_ref().into(), &trie, elevate)?;
    // host_println!("Mounted NFS subdirectories:\r\n{}", trie);
    Ok(())
}

fn parallel_unmount_recursive(trie: &dirtrie::Node) -> anyhow::Result<()> {
    trie.children
        .par_iter()
        .try_for_each(|(_, child)| parallel_unmount_recursive(child))?;

    if let Some((_, mount_path)) = &trie.paths {
        let shell_script = format!("diskutil unmount \"{}\"", mount_path);
        // host_println!("Running NFS unmount command: `{}`", &shell_script);
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

    for subdir in subdirs {
        let subdir = subdir.as_bytes();
        let subdir_relative = subdir
            .strip_prefix(mnt_point_base.as_ref().as_bytes())
            .and_then(|s| s.strip_prefix(b"/"))
            .unwrap_or(b"");

        trie.insert(&*subdir_relative.to_path_lossy(), subdir.into());
    }

    parallel_unmount_recursive(&trie)?;
    Ok(())
}

pub fn wait_for_file(file: impl AsRef<Path>) -> anyhow::Result<()> {
    let start = Instant::now();
    while !file.as_ref().exists() {
        if start.elapsed() > Duration::from_secs(5) {
            return Err(anyhow!(
                "Timeout waiting for file creation: {}",
                file.as_ref().display()
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Ok(())
}
