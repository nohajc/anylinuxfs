#![allow(non_camel_case_types)]

use libc::{c_char, c_int};

#[link(name = "krun", kind = "static")]
unsafe extern "C" {
    pub fn krun_set_log_level(level: u32) -> i32;
    pub fn krun_create_ctx() -> i32;
    pub fn krun_free_ctx(ctx: u32) -> i32;
    pub fn krun_set_vm_config(ctx: u32, num_vcpus: u8, ram_mib: u32) -> i32;
    pub fn krun_set_root(ctx: u32, root_path: *const c_char) -> i32;
    pub fn krun_add_disk(
        ctx_id: u32,
        block_id: *const c_char,
        disk_path: *const c_char,
        read_only: bool,
    ) -> i32;
    pub fn krun_set_mapped_volumes(ctx: u32, mapped_volumes: *const *const c_char) -> i32;
    pub fn krun_add_virtiofs(ctx_id: u32, c_tag: *const c_char, c_path: *const c_char) -> i32;
    pub fn krun_set_gvproxy_path(ctx: u32, c_path: *const c_char) -> i32;
    pub fn krun_set_port_map(ctx: u32, port_map: *const *const c_char) -> i32;
    pub fn krun_set_workdir(ctx: u32, workdir_path: *const c_char) -> i32;
    pub fn krun_set_exec(
        ctx: u32,
        exec_path: *const c_char,
        argv: *const *const c_char,
        envp: *const *const c_char,
    ) -> i32;
    pub fn krun_set_kernel(
        ctx_id: u32,
        kernel_path: *const c_char,
        kernel_format: u32,
        initramfs: *const c_char,
        cmdline: *const c_char,
    ) -> i32;
    pub fn krun_set_env(ctx: u32, envp: *const *const c_char) -> i32;
    pub fn krun_add_vsock_port2(
        ctx_id: u32,
        port: u32,
        unix_sock_path: *const c_char,
        listen: bool,
    ) -> i32;
    pub fn krun_setuid(ctx_id: u32, uid: libc::uid_t) -> i32;
    pub fn krun_setgid(ctx_id: u32, gid: libc::gid_t) -> i32;
    pub fn krun_start_enter(ctx: u32) -> i32;

    pub fn krun_add_disk_with_custom_io(
        ctx_id: u32,
        block_id: *const c_char,
        handle: c_int,
        preadv_fn: krun_preadv_fn_t,
        pwritev_fn: krun_pwritev_fn_t,
        size_fn: krun_size_fn_t,
        read_only: bool,
    ) -> i32;
}

pub type krun_preadv_fn_t = unsafe extern "C" fn(
    hnd: c_int,
    iov: *const libc::iovec,
    iovcnt: c_int,
    offset: libc::off_t,
) -> libc::ssize_t;

pub type krun_pwritev_fn_t = unsafe extern "C" fn(
    hnd: c_int,
    iov: *const libc::iovec,
    iovcnt: c_int,
    offset: libc::off_t,
) -> libc::ssize_t;

pub type krun_size_fn_t = unsafe extern "C" fn(hnd: c_int) -> libc::ssize_t;
