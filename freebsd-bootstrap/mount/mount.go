// Code borrowed and adapted from https://github.com/moby/sys (removed cgo dependency)
package mount

import (
	"strconv"
	"strings"
	"unsafe"

	"golang.org/x/sys/unix"
)

func Mount(device, target, mType, options string) error {
	flag, data := parseOptions(options)
	return mount(device, target, mType, uintptr(flag), data)
}

func allocateIOVecs(options []string) ([]unix.Iovec, [][]byte) {
	iovecs := make([]unix.Iovec, len(options))
	buffers := make([][]byte, len(options))
	for i, opt := range options {
		// NUL-terminate each string per nmount expectation.
		b := append([]byte(opt), 0)
		buffers[i] = b
		iovecs[i] = unix.Iovec{
			Base: &b[0],
			Len:  uint64(len(b)),
		}
	}
	return iovecs, buffers
}

func mount(device, target, mType string, flag uintptr, data string) error {
	isNullFS := false
	for x := range strings.SplitSeq(data, ",") {
		if x == "bind" {
			isNullFS = true
			break
		}
	}

	options := []string{"fspath", target}
	if isNullFS {
		options = append(options, "fstype", "nullfs", "target", device)
	} else {
		options = append(options, "fstype", mType, "from", device)
	}

	iovecs, _ := allocateIOVecs(options)

	// Perform raw syscall: int nmount(struct iovec *iov, unsigned int iovcnt, int flags);
	_, _, errno := unix.Syscall(unix.SYS_NMOUNT,
		uintptr(unsafe.Pointer(&iovecs[0])),
		uintptr(len(iovecs)),
		flag)
	if errno != 0 {
		return &mountError{
			op:     "mount",
			source: device,
			target: target,
			flags:  flag,
			err:    errno,
		}
	}
	return nil
}

// Parse fstab type mount options into mount() flags
// and device specific data
func parseOptions(options string) (int, string) {
	var (
		flag int
		data []string
	)

	for o := range strings.SplitSeq(options, ",") {
		// If the option does not exist in the flags table or the flag
		// is not supported on the platform,
		// then it is a data value for a specific fs type
		if f, exists := flags[o]; exists && f.flag != 0 {
			if f.clear {
				flag &= ^f.flag
			} else {
				flag |= f.flag
			}
		} else {
			data = append(data, o)
		}
	}
	return flag, strings.Join(data, ",")
}

var flags = map[string]struct {
	clear bool
	flag  int
}{
	"defaults":      {false, 0},
	"ro":            {false, RDONLY},
	"rw":            {true, RDONLY},
	"suid":          {true, NOSUID},
	"nosuid":        {false, NOSUID},
	"dev":           {true, NODEV},
	"nodev":         {false, NODEV},
	"exec":          {true, NOEXEC},
	"noexec":        {false, NOEXEC},
	"sync":          {false, SYNCHRONOUS},
	"async":         {true, SYNCHRONOUS},
	"dirsync":       {false, DIRSYNC},
	"remount":       {false, REMOUNT},
	"mand":          {false, MANDLOCK},
	"nomand":        {true, MANDLOCK},
	"atime":         {true, NOATIME},
	"noatime":       {false, NOATIME},
	"diratime":      {true, NODIRATIME},
	"nodiratime":    {false, NODIRATIME},
	"bind":          {false, BIND},
	"rbind":         {false, RBIND},
	"unbindable":    {false, UNBINDABLE},
	"runbindable":   {false, RUNBINDABLE},
	"private":       {false, PRIVATE},
	"rprivate":      {false, RPRIVATE},
	"shared":        {false, SHARED},
	"rshared":       {false, RSHARED},
	"slave":         {false, SLAVE},
	"rslave":        {false, RSLAVE},
	"relatime":      {false, RELATIME},
	"norelatime":    {true, RELATIME},
	"strictatime":   {false, STRICTATIME},
	"nostrictatime": {true, STRICTATIME},
}

const (
	// RDONLY will mount the filesystem as read-only.
	RDONLY = unix.MNT_RDONLY

	// NOSUID will not allow set-user-identifier or set-group-identifier bits to
	// take effect.
	NOSUID = unix.MNT_NOSUID

	// NOEXEC will not allow execution of any binaries on the mounted file system.
	NOEXEC = unix.MNT_NOEXEC

	// SYNCHRONOUS will allow any I/O to the file system to be done synchronously.
	SYNCHRONOUS = unix.MNT_SYNCHRONOUS

	// NOATIME will not update the file access time when reading from a file.
	NOATIME = unix.MNT_NOATIME
)

// These flags are unsupported.
const (
	BIND        = 0
	DIRSYNC     = 0
	MANDLOCK    = 0
	NODEV       = 0
	NODIRATIME  = 0
	UNBINDABLE  = 0
	RUNBINDABLE = 0
	PRIVATE     = 0
	RPRIVATE    = 0
	SHARED      = 0
	RSHARED     = 0
	SLAVE       = 0
	RSLAVE      = 0
	RBIND       = 0
	RELATIME    = 0
	REMOUNT     = 0
	STRICTATIME = 0
)

// mountError records an error from mount or unmount operation
type mountError struct {
	op             string
	source, target string
	flags          uintptr
	data           string
	err            error
}

func (e *mountError) Error() string {
	out := e.op + " "

	if e.source != "" {
		out += e.source + ":" + e.target
	} else {
		out += e.target
	}

	if e.flags != uintptr(0) {
		out += ", flags: 0x" + strconv.FormatUint(uint64(e.flags), 16)
	}
	if e.data != "" {
		out += ", data: " + e.data
	}

	out += ": " + e.err.Error()
	return out
}

// Cause returns the underlying cause of the error.
// This is a convention used in github.com/pkg/errors
func (e *mountError) Cause() error {
	return e.err
}

// Unwrap returns the underlying error.
// This is a convention used in golang 1.13+
func (e *mountError) Unwrap() error {
	return e.err
}
