return {
    kernel = {
        path = "freebsd/kernel.bin",
        cmdline = "FreeBSD:" ..
            "vfs.root.mountfrom=ufs:/dev/gpt/rootfs" ..
            -- "vfs.root.mountfrom=ufs:/dev/vtbd0p1" ..
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
        -- efi = true,
    },
    root = {
        type = "disk",
        path = "FreeBSD-14.3-RELEASE-arm64-aarch64-ufs.raw",
        -- path = "freebsd-microvm-disk.img",
        read_only = false,
    },
    data_disks = {
        -- { id = "data", path = "freebsd-microvm-disk.img", read_only = false },
        -- { id = "data2", path = "freebsd-oci.iso",          read_only = false },
    },
    workdir = "/",
    command = {
        path = "/bin/sh",
        args = { "-c", "/start-shell.sh" },
    },
    env = {
        -- "KRUN_ROOT=/dev/vda",
    },
    log_level = 0,
}
