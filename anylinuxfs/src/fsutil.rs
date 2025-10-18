use anyhow::anyhow;
use common_utils::host_println;
use rayon::prelude::*;
use std::{
    collections::HashSet,
    ffi::{CStr, CString, OsStr, OsString},
    io, mem,
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
    process::Command,
    ptr::null_mut,
};

#[derive(Debug, Clone)]
pub struct MountTable {
    disks: HashSet<OsString>,
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
        for buf in mounts_raw {
            let mntfromname = os_str_from_c_chars(&buf.f_mntfromname).to_owned();
            let mntonname = os_str_from_c_chars(&buf.f_mntonname).to_owned();
            // println!("mntfromname: {:?}", mntfromname);
            // println!("mntonname: {:?}", mntonname);

            if !mntfromname.is_empty() && !mntonname.is_empty() {
                disks.insert(mntfromname);
            }
        }
        Ok(MountTable { disks })
    }

    pub fn is_mounted(&self, path: impl AsRef<Path>) -> bool {
        let path = path.as_ref();
        self.disks.contains(path.as_os_str())
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
    let c_path = CString::new(path.as_ref().as_os_str().as_bytes()).unwrap();
    let mut buf: libc::statfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statfs(c_path.as_ptr(), &mut buf) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(buf)
}

fn os_str_from_c_chars(chars: &[i8]) -> &OsStr {
    let cstr = unsafe { CStr::from_ptr(chars.as_ptr()) };
    OsStr::from_bytes(cstr.to_bytes())
}

mod dirtrie {
    use std::{collections::BTreeMap, ffi::OsString, fmt::Display, path::Path};

    #[derive(Debug, Default)]
    pub struct Node {
        pub paths: Option<(OsString, String)>,
        pub children: BTreeMap<OsString, Node>,
    }

    impl Node {
        pub fn insert(&mut self, path: &Path, full_path: &str) {
            let mut current = self;
            for segment in path.components() {
                let segment = segment.as_os_str().to_owned();
                current = current.children.entry(segment).or_default();
            }
            current.paths = Some((path.as_os_str().to_owned(), full_path.to_owned()));
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
                            .unwrap_or("".to_owned())
                    )?;
                    fmt_node(child, f, &format!("{}--", prefix))?;
                }
                Ok(())
            }
            fmt_node(self, f, "")
        }
    }
}

fn parallel_mount_recursive(mnt_point_base: PathBuf, trie: &dirtrie::Node) -> anyhow::Result<()> {
    if let Some((rel_path, nfs_path)) = &trie.paths {
        let shell_script = format!(
            "mount -t nfs \"localhost:{}\" \"{}\"",
            nfs_path,
            mnt_point_base.join(rel_path).display()
        );
        host_println!("Running NFS mount command: `{}`", &shell_script);
        // TODO: elevate if needed (e.g. mounting image under /Volumes)
        let status = Command::new("sh")
            .arg("-c")
            .arg(&shell_script)
            // .stdout(Stdio::null())
            // .stderr(Stdio::null())
            .status()?; // TODO: make sure any error is properly printed

        if !status.success() {
            return Err(anyhow!(
                "mount failed with exit code {}",
                status
                    .code()
                    .map(|c| c.to_string())
                    .unwrap_or("unknown".to_owned())
            ));
        }
    }
    trie.children
        .par_iter()
        .try_for_each(|(_, child)| parallel_mount_recursive(mnt_point_base.clone(), child))?;

    Ok(())
}

pub fn mount_nfs_subdirs<'a>(
    share_path_base: &str,
    subdirs: impl Iterator<Item = &'a str>,
    mnt_point_base: impl AsRef<Path>,
) -> anyhow::Result<()> {
    let mut trie = dirtrie::Node::default();
    // TODO: try if mounting in parallel is faster
    // but make sure the order is correct:
    // - we'd need to construct a trie of all subdirs
    //   where each node corresponds to a path segment
    // - each node mounts its own subdir prefix path
    // - then repeats recursively for all children at once
    for subdir in subdirs {
        let subdir_relative = subdir
            .trim_start_matches(share_path_base)
            .trim_start_matches('/');

        trie.insert(Path::new(subdir_relative), subdir);
    }

    parallel_mount_recursive(mnt_point_base.as_ref().into(), &trie)?;
    host_println!("Mounted NFS subdirectories:\r\n{}", trie);
    Ok(())
}
