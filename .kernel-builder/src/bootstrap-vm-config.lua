return {
    kernel = {
        path = "/opt/homebrew/opt/anylinuxfs/libexec/Image",
    },
    vm = {
        vcpus = 1,
        ram_mib = 512,
    },
    root = {
        type = "path",
        path = "alpine-latest/rootfs",
        read_only = false,
    },
    data_disks = {
        { id = "data", path = "debian-bookworm-slim.img", read_only = false },
    },
    workdir = "/",
    command = {
        path = "/bin/sh",
        args = { "-c", '/bootstrap-vm-script.sh' },
    },
    env = {}
}
