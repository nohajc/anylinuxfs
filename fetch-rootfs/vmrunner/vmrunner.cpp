#include <cerrno>
#include <cstring>
#include <cstdio>

extern "C" {
#include <libkrun.h>
}

#include "vmrunner.h"

error success() {
    return error{0, nullptr, nullptr};
}

error make_err(int err, const char* prefix) {
    if (err < 0) {
        return error{-err, prefix, strerror(-err)};
    }
    return success();
}

struct krun_error : error {
    explicit krun_error(error e) : error(e) {}
    explicit krun_error(int err, const char* prefix = "") : krun_error{make_err(err, prefix)} {}
};

bool is_error(int res) {
    return res < 0;
}

error setup_and_start_vm(const char* root_path, const char* script_path) {
    int ctx = krun_create_ctx();
    if (is_error(ctx)) {
        return krun_error{ctx, "configuration context error"};
    }

    int res = krun_set_vm_config(ctx, 1, 512);
    if (is_error(res)) {
        return krun_error{res, "vm configuration error"};
    }

    res = krun_set_root(ctx, root_path);
    if (is_error(res)) {
        return krun_error{res, "set root error"};
    }

    res = krun_set_workdir(ctx, "/");
    if (is_error(res)) {
        return krun_error{res, "set workdir error"};
    }

    const char *const envp[] = { nullptr };
    const char *const argv[] = { "/bin/busybox", "sh", script_path, nullptr };

    res = krun_set_exec(ctx, argv[0], &argv[1], envp);
    if (is_error(res)) {
        return krun_error{res, "set exec error"};
    }

    res = krun_start_enter(ctx);
    if (is_error(res)) {
        return krun_error{res, "start vm error"};
    }

    return success();
}
