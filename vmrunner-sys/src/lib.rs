use std::os::raw::{c_char, c_int};
use std::ptr;

use krun::{
    krun_create_ctx, krun_set_exec, krun_set_kernel, krun_set_root, krun_set_vm_config,
    krun_set_workdir, krun_start_enter,
};

#[repr(C)]
pub struct Error {
    pub code: c_int,
    pub prefix: *const c_char,
    pub msg: *const c_char,
}

fn success() -> Error {
    Error { code: 0, prefix: ptr::null(), msg: ptr::null() }
}

fn krun_error(err: i32, prefix: &'static std::ffi::CStr) -> Error {
    Error {
        code: -err,
        prefix: prefix.as_ptr(),
        msg: unsafe { libc::strerror(-err) },
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn setup_and_start_vm(
    kernel_path: *const c_char,
    root_path: *const c_char,
    script_path: *const c_char,
) -> Error {
    let ctx = krun_create_ctx();
    if ctx < 0 {
        return krun_error(ctx, c"configuration context error");
    }
    let ctx = ctx as u32;

    let res = krun_set_vm_config(ctx, 1, 512);
    if res < 0 {
        return krun_error(res, c"vm configuration error");
    }

    let res = unsafe { krun_set_root(ctx, root_path) };
    if res < 0 {
        return krun_error(res, c"set root error");
    }

    let res = unsafe { krun_set_workdir(ctx, c"/".as_ptr()) };
    if res < 0 {
        return krun_error(res, c"set workdir error");
    }

    let envp: [*const c_char; 1] = [ptr::null()];
    let argv: [*const c_char; 3] = [c"sh".as_ptr(), script_path, ptr::null()];
    let res = unsafe { krun_set_exec(ctx, c"/bin/busybox".as_ptr(), argv.as_ptr(), envp.as_ptr()) };
    if res < 0 {
        return krun_error(res, c"set exec error");
    }

    let res = unsafe { krun_set_kernel(ctx, kernel_path, 0, ptr::null(), ptr::null()) };
    if res < 0 {
        return krun_error(res, c"set kernel error");
    }

    let res = krun_start_enter(ctx);
    if res < 0 {
        return krun_error(res, c"start vm error");
    }

    success()
}
