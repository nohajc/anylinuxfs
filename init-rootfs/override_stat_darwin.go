//go:build darwin

package main

import (
	"fmt"
	"io/fs"
	"os"
	"path/filepath"

	"golang.org/x/sys/unix"
)

const overrideStatXattr = "user.containers.override_stat"

// stampOverrideStat walks rootfsPath and writes user.containers.override_stat
// on every entry so libkrun's macOS virtiofs presents files to the guest as
// owned by uid 0, gid 0 with their real permission bits. Uses the 3-field
// form ("0:0:0<octal>"); libkrun ORs in the host's real type bits at read
// time so this value is correct for files, dirs, and symlinks alike.
func stampOverrideStat(rootfsPath string) error {
	return filepath.WalkDir(rootfsPath, func(path string, d fs.DirEntry, walkErr error) error {
		if walkErr != nil {
			return walkErr
		}
		info, err := os.Lstat(path)
		if err != nil {
			return fmt.Errorf("lstat %s: %w", path, err)
		}
		m := info.Mode()
		mode := uint32(m.Perm())
		if m&os.ModeSetuid != 0 {
			mode |= 0o4000
		}
		if m&os.ModeSetgid != 0 {
			mode |= 0o2000
		}
		if m&os.ModeSticky != 0 {
			mode |= 0o1000
		}
		value := fmt.Sprintf("0:0:0%o", mode)
		if err := lsetxattrWithWriteAccess(path, m, value); err != nil {
			return fmt.Errorf("lsetxattr %s: %w", path, err)
		}
		return nil
	})
}

// lsetxattrWithWriteAccess sets the override_stat xattr, working around the
// macOS requirement that setxattr needs write permission on the target. For
// regular files and directories that lack the owner-write bit (e.g. /var/empty
// at mode 0555), temporarily adds owner-write, sets the xattr, then restores
// the original mode. Symlinks are exempt — Lsetxattr passes XATTR_NOFOLLOW so
// it operates on the link inode (mode 0755 by default on macOS).
func lsetxattrWithWriteAccess(path string, m os.FileMode, value string) error {
	// Lsetxattr -> setxattr with XATTR_NOFOLLOW on macOS.
	// Flags=0 -> create-or-replace.
	err := unix.Lsetxattr(path, overrideStatXattr, []byte(value), 0)
	if err == nil {
		return nil
	}
	// Only fall back for regular files / dirs missing owner-write.
	if m&os.ModeSymlink != 0 || m.Perm()&0o200 != 0 {
		return err
	}
	relaxed := m.Perm() | 0o200
	if chmodErr := os.Chmod(path, relaxed); chmodErr != nil {
		return err // surface the original setxattr error
	}
	xattrErr := unix.Lsetxattr(path, overrideStatXattr, []byte(value), 0)
	// Restore original mode regardless of xattr outcome.
	if restoreErr := os.Chmod(path, m.Perm()); restoreErr != nil && xattrErr == nil {
		return restoreErr
	}
	return xattrErr
}
