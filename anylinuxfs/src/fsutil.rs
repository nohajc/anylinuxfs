use std::{
    ffi::{CStr, CString, OsStr},
    io,
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
};

pub fn mounted_from(path: impl AsRef<Path>) -> io::Result<PathBuf> {
    let c_path = CString::new(path.as_ref().as_os_str().as_bytes()).unwrap();
    let mut buf: libc::statfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statfs(c_path.as_ptr(), &mut buf) } != 0 {
        return Err(io::Error::last_os_error());
    }

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

fn os_str_from_c_chars(chars: &[i8]) -> &OsStr {
    let cstr = unsafe { CStr::from_ptr(chars.as_ptr()) };
    OsStr::from_bytes(cstr.to_bytes())
}
