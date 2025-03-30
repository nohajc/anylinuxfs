package vmrunner

/*
#include <stdlib.h>
#include "vmrunner.h"
#cgo LDFLAGS: -lkrun
*/
import "C"
import (
	"fmt"
	"unsafe"
)

func Run(kernelPath, rootPath, scriptPath string) error {
	cKernelPath := C.CString(kernelPath)
	defer C.free(unsafe.Pointer(cKernelPath))

	cRootPath := C.CString(rootPath)
	defer C.free(unsafe.Pointer(cRootPath))

	cScriptPath := C.CString(scriptPath)
	defer C.free(unsafe.Pointer(cScriptPath))

	cerr := C.setup_and_start_vm(cKernelPath, cRootPath, cScriptPath)
	if cerr.code != 0 {
		return fmt.Errorf(
			"%s: %s (errno %d)",
			C.GoString(cerr.prefix),
			C.GoString(cerr.msg),
			cerr.code)
	}
	return nil
}
