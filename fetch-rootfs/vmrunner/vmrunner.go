package vmrunner

/*
#include <stdlib.h>
#include "vmrunner.h"
#cgo CXXFLAGS: -std=c++20
#cgo LDFLAGS: -lkrun
*/
import "C"
import (
	"fmt"
	"unsafe"
)

func Run(rootPath, scriptPath string) error {
	cRootPath := C.CString(rootPath)
	defer C.free(unsafe.Pointer(cRootPath))
	cScriptPath := C.CString(scriptPath)
	defer C.free(unsafe.Pointer(cScriptPath))

	cerr := C.setup_and_start_vm(cRootPath, cScriptPath)
	if cerr.code != 0 {
		return fmt.Errorf(
			"%s: %s (errno %d)",
			C.GoString(cerr.prefix),
			C.GoString(cerr.msg),
			cerr.code)
	}
	return nil
}
