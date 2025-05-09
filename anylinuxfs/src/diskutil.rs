use anyhow::Context;
use common_utils::host_println;
use libc::{SIGINT, SIGTERM};
use nix::sys::signal::Signal;
use objc2_core_foundation::{
    CFDictionary, CFRetained, CFRunLoop, CFString, CFURL, kCFRunLoopDefaultMode,
};
use objc2_disk_arbitration::{
    DADisk, DARegisterDiskAppearedCallback, DARegisterDiskDisappearedCallback, DASession,
    DAUnregisterCallback,
};
use regex::Regex;
use signal_hook::iterator::Signals;
use std::{
    ffi::c_void,
    fmt::Display,
    marker::PhantomData,
    ops::Deref,
    path::Path,
    process::Command,
    ptr::{NonNull, null, null_mut},
    thread,
};
use url::Url;

use crate::{devinfo::DevInfo, fsutil};

pub struct Entry(String, String, Vec<String>);

impl Entry {
    pub fn new(disk: &str) -> Self {
        Entry(disk.to_owned(), String::default(), Vec::new())
    }

    pub fn disk(&self) -> &str {
        self.0.as_str()
    }

    pub fn header(&self) -> &str {
        self.1.as_str()
    }

    pub fn header_mut(&mut self) -> &mut String {
        &mut self.1
    }

    pub fn partitions(&self) -> &[String] {
        &self.2
    }

    pub fn partitions_mut(&mut self) -> &mut Vec<String> {
        &mut self.2
    }
}

pub struct List(Vec<Entry>);

impl Display for List {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for entry in &self.0 {
            if entry.partitions().is_empty() {
                continue;
            }
            writeln!(f, "{}", entry.disk())?;
            if !entry.header().is_empty() {
                writeln!(f, "{}", entry.header())?;
            }
            for partition in entry.partitions() {
                writeln!(f, "{}", partition)?;
            }
        }
        Ok(())
    }
}

fn trunc_with_ellipsis(s: &str, max_len: usize) -> String {
    if s.len() > max_len {
        format!("{}...", &s[..max_len - 3])
    } else {
        s.to_string()
    }
}

pub fn list_linux_partitions() -> anyhow::Result<List> {
    let part_type_pattern = Regex::new(r"(Linux Filesystem|Linux)").unwrap();
    let mut disk_entries = Vec::new();

    let output = Command::new("diskutil")
        .arg("list")
        .output()
        .expect("Failed to execute diskutil");

    if !output.status.success() {
        return Err(anyhow::anyhow!("diskutil command failed"));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut current_entry = None;

    for line in stdout.lines() {
        if line.starts_with("/dev/disk") {
            disk_entries.push(Entry::new(line));
            let last_idx = disk_entries.len() - 1;
            current_entry = disk_entries.get_mut(last_idx)
        } else if line.trim_start().starts_with("#:") {
            current_entry.as_mut().map(|entry| {
                entry.header_mut().push_str(line);
            });
        } else {
            if let Some(part_type) = part_type_pattern.find(line).map(|m| m.as_str()) {
                let dev_info = line
                    .split_whitespace()
                    .last()
                    .map(|part| DevInfo::new(&format!("/dev/{part}")).ok())
                    .flatten();
                let line = match dev_info {
                    Some(dev_info) => {
                        let mut line = line.to_owned();
                        let fs_type = dev_info.fs_type().unwrap_or(part_type);
                        let label = trunc_with_ellipsis(
                            dev_info.label().unwrap_or("                       "),
                            23,
                        );
                        line = line.replace(
                            &format!("{:>26} {:<23}", part_type, ""),
                            &format!("{:>26} {:<23}", fs_type, label),
                        );

                        line
                    }
                    None => line.to_owned(),
                };
                current_entry.as_mut().map(|entry| {
                    entry.partitions_mut().push(line);
                });
            }
        }
    }
    Ok(List(disk_entries))
}

unsafe fn cfdict_get_value<'a, T>(dict: &'a CFDictionary, key: &str) -> Option<&'a T> {
    let key = CFString::from_str(key);
    let key_ptr: *const CFString = key.deref();
    let mut value_ptr: *const c_void = null();
    let key_found = unsafe { dict.value_if_present(key_ptr as *const c_void, &mut value_ptr) };

    if !key_found {
        return None;
    }
    unsafe { (value_ptr as *const T).as_ref() }
}

struct DaDiskArgs<ContextType> {
    context: *mut c_void,
    descr: Option<CFRetained<CFDictionary>>,
    phantom: PhantomData<ContextType>,
}

impl<ContextType> DaDiskArgs<ContextType> {
    fn new(disk: NonNull<DADisk>, context: *mut c_void) -> Self {
        let descr = unsafe { DADisk::description(disk.as_ref()) };
        Self {
            context,
            descr,
            phantom: PhantomData,
        }
    }

    fn context(&self) -> &ContextType {
        unsafe { (self.context as *const ContextType).as_ref().unwrap() }
    }

    fn context_mut(&mut self) -> &mut ContextType {
        unsafe { (self.context as *mut ContextType).as_mut().unwrap() }
    }

    fn descr(&self) -> Option<&CFDictionary> {
        self.descr.as_ref().map(|d| d.deref())
    }

    fn volume_path(&self) -> Option<String> {
        let volume_path: Option<&CFURL> =
            unsafe { cfdict_get_value(self.descr()?, "DAVolumePath") };
        volume_path
            .map(|url| CFURL::string(url).to_string())
            .and_then(|url_str| Url::parse(&url_str).ok())
            .map(|url| url.path().to_string())
    }

    fn volume_kind(&self) -> Option<String> {
        let volume_kind: Option<&CFString> =
            unsafe { cfdict_get_value(self.descr()?, "DAVolumeKind") };
        volume_kind.map(|kind| kind.to_string())
    }
}

unsafe extern "C-unwind" fn disk_mount_event(disk: NonNull<DADisk>, context: *mut c_void) {
    let mut args = DaDiskArgs::<MountContext>::new(disk, context);

    if let (Some(volume_path), Some(volume_kind)) = (args.volume_path(), args.volume_kind()) {
        let expected_nfs_path = args.context().nfs_path;
        if volume_kind == "nfs" {
            if let Ok(dev_path) = fsutil::mounted_from(&volume_path) {
                if dev_path == expected_nfs_path {
                    args.context_mut().real_mount_point = Some(volume_path.clone());
                    CFRunLoop::stop(&CFRunLoop::main().unwrap());
                }
            }
        }
    }
}

unsafe extern "C-unwind" fn disk_unmount_event(disk: NonNull<DADisk>, context: *mut c_void) {
    let args = DaDiskArgs::<UnmountContext>::new(disk, context);

    if let (Some(volume_path), Some(volume_kind)) = (args.volume_path(), args.volume_kind()) {
        let expected_mount_point = args.context().mount_point;
        if volume_kind == "nfs" && volume_path == expected_mount_point {
            CFRunLoop::stop(&CFRunLoop::main().unwrap());
        }
    }
}

// unsafe extern "C-unwind" fn disk_unmount_approval(
//     disk: NonNull<DADisk>,
//     context: *mut c_void,
// ) -> *const DADissenter {
//     let args = DaDiskArgs::new(disk, context);
//     if let Some(descr) = args.descr() {
//         inspect_cf_dictionary_values(descr);
//     }
//     if let (Some(volume_path), Some(volume_kind)) = (args.volume_path(), args.volume_kind()) {
//         let expected_share_path = format!("/Volumes/{}/", args.share_name());
//         if volume_kind == "nfs" && volume_path == expected_share_path {
//             host_println!("Approve unmount of {}? [y/n]", &expected_share_path);
//             let mut input = String::new();
//             io::stdin().read_line(&mut input).unwrap();
//             if input.trim() == "y" {
//                 return null();
//             }
//         }
//     }
//     let msg = CFString::from_str("custom error message");
//     let result = unsafe { DADissenterCreate(None, kDAReturnBusy, Some(&msg)) };
//     msg.retain();
//     result.retain();
//     result.deref()
// }

// fn inspect_cf_dictionary_values(dict: &CFDictionary) {
//     let count = unsafe { CFDictionaryGetCount(dict) } as usize;
//     let mut keys: Vec<*const c_void> = vec![null(); count];
//     let mut values: Vec<*const c_void> = vec![null(); count];

//     unsafe { CFDictionaryGetKeysAndValues(dict, keys.as_mut_ptr(), values.as_mut_ptr()) };

//     for i in 0..count {
//         let value = values[i] as *const CFType;
//         let type_id = unsafe { CFGetTypeID(value.as_ref()) };
//         let type_name = CFCopyTypeIDDescription(type_id).unwrap();
//         let key_str = keys[i] as *const CFString;

//         host_println!(
//             "Key: {}, Type: {}",
//             unsafe { key_str.as_ref().unwrap() },
//             &type_name,
//         );
//     }
// }

struct MountContext<'a> {
    nfs_path: &'a Path,
    // what the OS assigned after resolving any potential conflicts
    real_mount_point: Option<String>,
}

impl<'a> MountContext<'a> {
    fn new(nfs_path: &'a Path) -> Self {
        Self {
            nfs_path,
            real_mount_point: None,
        }
    }
}

struct UnmountContext<'a> {
    mount_point: &'a str,
}

fn stop_run_loop_on_signal() -> anyhow::Result<()> {
    let mut signals = Signals::new(&[SIGINT, SIGTERM]).context("failed to register signals")?;
    _ = thread::spawn(move || {
        for signal in signals.forever() {
            match signal {
                SIGINT | SIGTERM => {
                    host_println!(
                        "Received signal {}",
                        Signal::try_from(signal)
                            .map(|s| s.to_string())
                            .unwrap_or("<unknown>".to_owned())
                    );
                    CFRunLoop::stop(&CFRunLoop::main().unwrap());
                    break;
                }
                _ => {}
            }
        }
    });

    Ok(())
}

pub struct MountPoint(String);

impl MountPoint {
    pub fn real(&self) -> &str {
        self.0.as_str()
    }

    // macOS disk events contain mount points with trailing slashes
    // however, we want to remove them for display purposes
    pub fn display(&self) -> &str {
        self.0.as_str().trim_end_matches('/')
    }
}

pub struct EventSession {
    session: CFRetained<DASession>,
}

impl EventSession {
    pub fn new() -> anyhow::Result<Self> {
        let session = unsafe { DASession::new(None).unwrap() };
        stop_run_loop_on_signal()?;
        Ok(Self { session })
    }

    // returns None when interrupted by SIGINT/SIGTERM
    pub fn wait_for_mount(&self, nfs_path: &Path) -> Option<MountPoint> {
        let mut mount_ctx = MountContext::new(nfs_path);
        let mount_ctx_ptr = &mut mount_ctx as *mut MountContext;
        unsafe {
            DARegisterDiskAppearedCallback(
                &self.session,
                None,
                Some(disk_mount_event),
                mount_ctx_ptr as *mut c_void,
            )
        };

        unsafe {
            DASession::schedule_with_run_loop(
                &self.session,
                &CFRunLoop::main().unwrap(),
                kCFRunLoopDefaultMode.unwrap(),
            )
        };

        CFRunLoop::run();

        let callback_ptr = disk_mount_event as *const c_void as *mut c_void;
        let callback_nonnull: NonNull<c_void> = NonNull::new(callback_ptr).unwrap();
        unsafe { DAUnregisterCallback(&self.session, callback_nonnull, null_mut()) };

        mount_ctx.real_mount_point.map(MountPoint)
    }

    pub fn wait_for_unmount(&self, mount_point: &str) {
        let mut unmount_ctx = UnmountContext { mount_point };
        let mount_ctx_ptr = &mut unmount_ctx as *mut UnmountContext;
        unsafe {
            DARegisterDiskDisappearedCallback(
                &self.session,
                None,
                Some(disk_unmount_event),
                mount_ctx_ptr as *mut c_void,
            )
        };

        // unsafe {
        //     DARegisterDiskEjectApprovalCallback(
        //         &session,
        //         None,
        //         Some(disk_unmount_approval),
        //         mount_ctx_ptr as *mut c_void,
        //     )
        // }

        unsafe {
            DASession::schedule_with_run_loop(
                &self.session,
                &CFRunLoop::main().unwrap(),
                kCFRunLoopDefaultMode.unwrap(),
            )
        };

        CFRunLoop::run();

        let callback_ptr = disk_unmount_event as *const c_void as *mut c_void;
        let callback_nonnull: NonNull<c_void> = NonNull::new(callback_ptr).unwrap();
        unsafe { DAUnregisterCallback(&self.session, callback_nonnull, null_mut()) };
    }
}
