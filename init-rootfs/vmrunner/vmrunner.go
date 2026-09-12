package vmrunner

/*
#include <stdlib.h>
#include "vmrunner.h"
#cgo LDFLAGS: ${SRCDIR}/../../vmrunner-sys/target/libvmrunner_sys.a
#cgo darwin LDFLAGS: -framework Hypervisor
*/
import "C"
import (
	"fmt"
	"unsafe"
)

func Run(kernelPath, rootPath, scriptPath string, env []string) error {
	cKernelPath := C.CString(kernelPath)
	defer C.free(unsafe.Pointer(cKernelPath))

	cRootPath := C.CString(rootPath)
	defer C.free(unsafe.Pointer(cRootPath))

	cScriptPath := C.CString(scriptPath)
	defer C.free(unsafe.Pointer(cScriptPath))

	cEnv := make([]*C.char, 0, len(env)+1)
	for _, entry := range env {
		cEntry := C.CString(entry)
		defer C.free(unsafe.Pointer(cEntry))
		cEnv = append(cEnv, cEntry)
	}
	cEnv = append(cEnv, nil)

	cerr := C.setup_and_start_vm(cKernelPath, cRootPath, cScriptPath, &cEnv[0])
	if cerr.code != 0 {
		return fmt.Errorf(
			"%s: %s (errno %d)",
			C.GoString(cerr.prefix),
			C.GoString(cerr.msg),
			cerr.code)
	}
	return nil
}
