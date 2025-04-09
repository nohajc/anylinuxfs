#pragma once

typedef struct error {
    int code;
    const char* prefix;
    const char* msg;
} error;

error setup_and_start_vm(const char* kernel_path, const char* root_path, const char* script_path);
