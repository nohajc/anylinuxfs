return {
    kernel = {
        path = "/opt/homebrew/opt/anylinuxfs/libexec/Image",
        -- cmdline = "reboot=k panic=-1 panic_print=0 console=hvc0 rootfstype=virtiofs rw quiet no-kvmapf init=/init.krun",
    },
    vm = {
        vcpus = 4,
        ram_mib = 2048,
    },
    root = {
        type = "disk",
        path = "debian-bookworm-slim.img",
        read_only = false,
    },
    workdir = "/",
    command = {
        path = "/bin/bash",
        args = { "-c", "/start-shell.sh" },
    },
    env = {
        -- "KRUN_ROOT=/dev/vda",
    },
    log_level = 0,
}
