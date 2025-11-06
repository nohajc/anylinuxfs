package chroot

import (
	"errors"
	"unsafe"

	"golang.org/x/sys/unix"
)

// Chroot performs a chroot(2) system call to change the root directory
// of the calling process to path. Requires appropriate privileges.
// Returns *chrootError on failure.
func Chroot(path string) error {
	if path == "" {
		return errors.New("chroot: empty path")
	}
	b := append([]byte(path), 0) // NUL terminate
	_, _, errno := unix.Syscall(unix.SYS_CHROOT, uintptr(unsafe.Pointer(&b[0])), 0, 0)
	if errno != 0 {
		return &chrootError{path: path, err: errno}
	}
	return nil
}

// chrootError captures details of a failed chroot syscall.
type chrootError struct {
	path string
	err  error
}

func (e *chrootError) Error() string {
	return "chroot " + e.path + ": " + e.err.Error()
}

// Cause exposes underlying error (pkg/errors convention).
func (e *chrootError) Cause() error { return e.err }

// Unwrap exposes underlying error (Go 1.13+ convention).
func (e *chrootError) Unwrap() error { return e.err }
