#include <errno.h>
#include <string.h>
#include <stdio.h>
#include <stdbool.h>

#include <libkrun.h>

#include "vmrunner.h"

error success() {
    error e = {0, NULL, NULL};
    return e;
}

error krun_error(int err, const char* prefix) {
    if (err < 0) {
        error e = {-err, prefix, strerror(-err)};
        return e;
    }
    return success();
}

bool is_error(int res) {
    return res < 0;
}

error setup_and_start_vm(const char* root_path, const char* script_path) {
    int ctx = krun_create_ctx();
    if (is_error(ctx)) {
        return krun_error(ctx, "configuration context error");
    }

    int res = krun_set_vm_config(ctx, 1, 512);
    if (is_error(res)) {
        return krun_error(res, "vm configuration error");
    }

    res = krun_set_root(ctx, root_path);
    if (is_error(res)) {
        return krun_error(res, "set root error");
    }

    res = krun_set_workdir(ctx, "/");
    if (is_error(res)) {
        return krun_error(res, "set workdir error");
    }

    const char *const envp[] = { NULL };
    const char *const argv[] = { "/bin/busybox", "sh", script_path, NULL };

    res = krun_set_exec(ctx, argv[0], &argv[1], envp);
    if (is_error(res)) {
        return krun_error(res, "set exec error");
    }

    // TODO: use kernel_path under configured prefix
    const char *kernel_path = "/Users/nohajan/gitprojs/3rd-party/libkrunfw/linux-6.6.59/arch/arm64/boot/Image";
    res = krun_set_kernel(ctx, kernel_path, 0, NULL, NULL);
    if (is_error(res)) {
        return krun_error(res, "set kernel error");
    }

    res = krun_start_enter(ctx);
    if (is_error(res)) {
        return krun_error(res, "start vm error");
    }

    return success();
}
