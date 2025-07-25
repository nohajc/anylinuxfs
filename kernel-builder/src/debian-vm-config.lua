return {
    kernel = {
        path = "/opt/homebrew/opt/anylinuxfs/libexec/Image",
        cmdline =
        "reboot=k panic=-1 panic_print=0 nomodule console=hvc0 root=/dev/vda rw quiet no-kvmapf init=/init.krun",
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
    env = {}
}
