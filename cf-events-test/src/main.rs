use objc2_core_foundation::{
    CFCopyTypeIDDescription, CFDictionary, CFDictionaryGetCount, CFDictionaryGetKeysAndValues,
    CFDictionaryGetValueIfPresent, CFGetTypeID, CFRunLoopGetCurrent, CFRunLoopRun, CFRunLoopStop,
    CFString, CFType, CFURL, CFURLGetString, kCFRunLoopDefaultMode,
};
use objc2_disk_arbitration::{
    DADisk, DADiskCopyDescription, DARegisterDiskDisappearedCallback, DASessionCreate,
    DASessionScheduleWithRunLoop, DAUnregisterCallback,
};
use std::{
    ffi::c_void,
    ops::Deref,
    ptr::{NonNull, null, null_mut},
};

fn inspect_cf_dictionary_values(dict: &CFDictionary) {
    let count = unsafe { CFDictionaryGetCount(dict) } as usize;
    let mut keys: Vec<*const c_void> = vec![null(); count];
    let mut values: Vec<*const c_void> = vec![null(); count];

    unsafe { CFDictionaryGetKeysAndValues(dict, keys.as_mut_ptr(), values.as_mut_ptr()) };

    for i in 0..count {
        let value = values[i] as *const CFType;
        let type_id = unsafe { CFGetTypeID(value.as_ref()) };
        let type_name = CFCopyTypeIDDescription(type_id).unwrap();
        let key_str = keys[i] as *const CFString;

        println!(
            "Key: {}, Type: {}",
            unsafe { key_str.as_ref().unwrap() },
            &type_name,
        );
    }
}

unsafe fn cfdict_get_value<'a, T>(dict: &'a CFDictionary, key: &str) -> Option<&'a T> {
    let key = CFString::from_str(key);
    let key_ptr: *const CFString = key.deref();
    let mut value_ptr: *const c_void = null();
    let key_found =
        unsafe { CFDictionaryGetValueIfPresent(dict, key_ptr as *const c_void, &mut value_ptr) };

    if !key_found {
        return None;
    }
    unsafe { (value_ptr as *const T).as_ref() }
}

unsafe extern "C-unwind" fn disk_unmount_event(disk: NonNull<DADisk>, _context: *mut c_void) {
    let disk = unsafe { disk.as_ref() };
    if let Some(descr) = unsafe { DADiskCopyDescription(disk) } {
        // println!("Disk unmounted: {:?}", &descr);
        // inspect_cf_dictionary_values(&descr);

        let volume_path: Option<&CFURL> = unsafe { cfdict_get_value(&descr, "DAVolumePath") };
        let volume_kind: Option<&CFString> = unsafe { cfdict_get_value(&descr, "DAVolumeKind") };

        if let Some(volume_path) = volume_path {
            let volume_path = unsafe { CFURLGetString(volume_path).unwrap() };
            println!("Volume path: {}", &volume_path);
        }

        if let Some(volume_kind) = volume_kind {
            println!("Volume kind: {}", &volume_kind);
        }

        unsafe { CFRunLoopStop(&CFRunLoopGetCurrent().unwrap()) };
    }
}

fn main() {
    let session = unsafe { DASessionCreate(None).unwrap() };
    unsafe {
        DARegisterDiskDisappearedCallback(&session, None, Some(disk_unmount_event), null_mut())
    };

    unsafe {
        DASessionScheduleWithRunLoop(
            &session,
            &CFRunLoopGetCurrent().unwrap(),
            kCFRunLoopDefaultMode.unwrap(),
        )
    };

    unsafe { CFRunLoopRun() };

    let callback_ptr = disk_unmount_event as *const c_void as *mut c_void;
    let callback_nonnull = NonNull::new(callback_ptr).unwrap();
    unsafe { DAUnregisterCallback(&session, callback_nonnull, null_mut()) };
}
