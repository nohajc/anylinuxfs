#pragma once

#ifdef __cplusplus
extern "C" {
#endif

typedef struct error {
    int code;
    const char* prefix;
    const char* msg;
} error;

error setup_and_start_vm(const char* root_path);

#ifdef __cplusplus
}
#endif
