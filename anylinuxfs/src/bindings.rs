use libc::{c_char, c_int};

#[link(name = "krun")]
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
    pub fn krun_set_gvproxy_path(ctx: u32, c_path: *const c_char) -> i32;
    pub fn krun_set_port_map(ctx: u32, port_map: *const *const c_char) -> i32;
    pub fn krun_set_workdir(ctx: u32, workdir_path: *const c_char) -> i32;
    pub fn krun_set_exec(
        ctx: u32,
        exec_path: *const c_char,
        argv: *const *const c_char,
        envp: *const *const c_char,
    ) -> i32;
    pub fn krun_set_env(ctx: u32, envp: *const *const c_char) -> i32;
    pub fn krun_add_vsock_port2(
        ctx_id: u32,
        port: u32,
        unix_sock_path: *const c_char,
        listen: bool,
    ) -> i32;
    pub fn krun_start_enter(ctx: u32) -> i32;
}
