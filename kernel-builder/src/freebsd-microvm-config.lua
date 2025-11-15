return {
    kernel = {
        path = "kernel/kernel.bin",
        cmdline = "FreeBSD:" ..
            "vfs.root.mountfrom=ufs:/dev/gpt/rootfs" ..
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
        path = "freebsd-microvm-disk.img",
        read_only = false,
    },
    data_disks = {
        -- { id = "data", path = "freebsd-microvm-disk.img", read_only = false },
    },
    workdir = "/",
    command = {
        path = "/bin/sh",
        args = { "-c", "/start-shell.sh" },
    },
    env = {
    },
    log_level = 0,
}
