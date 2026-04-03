return {
    kernel = {
        path = "kernel/kernel.bin",
        cmdline = "FreeBSD:" ..
            "vfs.root.mountfrom=cd9660:/dev/vtbd0" ..
            " kernel_path=/boot/kernel kernelname=/boot/kernel/kernel" ..
            " module_path=/boot/kernel;/boot/modules;/boot/dtb;/boot/dtb/overlays" ..
            " -mq" ..
            " init_path=/init-freebsd" ..
            " module_verbose=2",
    },
    vm = {
        vcpus = 4,
        ram_mib = 2048,
        legacy_console = true,
    },
    root = {
        type = "disk",
        path = "freebsd-bootstrap.iso",
        read_only = false,
    },
    data_disks = {
        { id = "data",  path = "freebsd-microvm-disk.img", read_only = false },
        { id = "data2", path = "freebsd-oci.iso",          read_only = false },
    },
    workdir = "/",
    command = {
        path = "/freebsd-bootstrap",
    },
    env = {
        -- "KRUN_ROOT=/dev/vda",
    },
    log_level = 0,
}
