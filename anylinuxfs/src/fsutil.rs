use std::{
    collections::HashSet,
    ffi::{CStr, CString, OsStr, OsString},
    io, mem,
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
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
