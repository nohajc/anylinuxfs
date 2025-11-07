package main

import (
	"anylinuxfs/freebsd-bootstrap/chroot"
	"anylinuxfs/freebsd-bootstrap/mount"
	"anylinuxfs/freebsd-bootstrap/oci"
	"anylinuxfs/freebsd-bootstrap/remoteiso"
	"debug/elf"
	_ "embed"
	"errors"
	"fmt"
	"net/http"
	"os"
	"os/exec"
	"path/filepath"
	"time"

	"github.com/kdomanski/iso9660"
)

const FreeBSD_ISO = "https://download.freebsd.org/releases/ISO-IMAGES/14.3/FreeBSD-14.3-RELEASE-arm64-aarch64-bootonly.iso"

var RequiredFiles = []string{
	"/lib/geom/geom_part.so",
	"/sbin/fsck_ffs",
	"/sbin/gpart",
	"/sbin/newfs",
	"/sbin/zfs",
	"/sbin/zpool",
	"/usr/sbin/nfsd", // TODO: the rest of NFS dependencies
}
var LibraryBaseDirs = []string{"/lib", "/usr/lib"}

func main() {
	fmt.Println("Bootstrap started")

	workdir := "tmp"
	if _, err := os.Stat(workdir); os.IsNotExist(err) {
		err := os.Mkdir(workdir, 0755)
		if err != nil {
			fmt.Printf("Failed to create workdir %s: %v\n", workdir, err)
			return
		}
	}
	err := mount.Mount("tmpfs", workdir, "tmpfs", "")
	if err != nil {
		fmt.Printf("Failed to mount tmpfs on %s: %v\n", workdir, err)
		return
	}
	fmt.Println("mounted tmpfs")

	copyInitBinary(workdir)

	// Switch to a temporary root populated from the ISO
	err = os.Chdir(workdir)
	if err != nil {
		fmt.Printf("Failed to change directory to %s: %v\n", workdir, err)
		return
	}
	err = chroot.Chroot(".")
	if err != nil {
		fmt.Printf("Failed to chroot into current directory: %v\n", err)
		return
	}
	workdir = "/"

	fmt.Println("chrooted to /tmp")

	err = os.Mkdir("/dev", 0755)
	if err != nil && !os.IsExist(err) {
		fmt.Printf("Failed to create /dev directory: %v\n", err)
		return
	}
	err = mount.Mount("devfs", "/dev", "devfs", "")
	if err != nil {
		fmt.Printf("Failed to mount devfs on /dev: %v\n", err)
		return
	}
	fmt.Println("mounted devfs")

	err = os.MkdirAll("/mnt/img", 0755)
	if err != nil && !os.IsExist(err) {
		fmt.Printf("Error creating /mnt/img: %v\n", err)
		return
	}

	ociDir := "/mnt/img"
	err = mount.Mount("/dev/vtbd2", ociDir, "cd9660", "")
	if err != nil {
		fmt.Printf("Error mounting /dev/vtbd2 to %s: %v\n", ociDir, err)
		return
	}
	fmt.Println("mounted OCI image")

	// TODO: get tag name dynamically by doing the equivalent of `umoci list`
	err = oci.Unpack(ociDir, "freebsd-runtime:14.3-RELEASE-aarch64", ".")
	if err != nil {
		fmt.Printf("Error unpacking OCI image: %v\n", err)
		return
	}
	fmt.Println("unpacked OCI image")

	err = initNetwork()
	if err != nil {
		fmt.Printf("Error initializing network: %v\n", err)
		return
	}
	fmt.Println("network initialized")

	err = createResolvConf("/")
	if err != nil {
		fmt.Printf("Error creating resolv.conf: %v\n", err)
		return
	}
	fmt.Println("created resolv.conf")

	reader := &remoteiso.HTTPReaderAt{
		URL:    FreeBSD_ISO,
		Client: &http.Client{},
	}

	cached := &remoteiso.CachedReaderAt{
		Base:      reader,
		BlockSize: 128 * 1024,
		Cache:     make(map[int64][]byte),
	}

	image, err := iso9660.OpenImage(cached)
	if err != nil {
		fmt.Printf("Failed to open ISO image %s: %v\n", FreeBSD_ISO, err)
		return
	}

	root, err := image.RootDir()
	if err != nil {
		fmt.Printf("Failed to get root directory of ISO: %v\n", err)
		return
	}

	fmt.Printf("Reading %s:\n\n", FreeBSD_ISO)

	start := time.Now()
	// listDir(root, "")

	foundFiles := remoteiso.FindFiles(root, RequiredFiles)
	d := newDownloader(workdir, root)
	d.downloadWithDependencies(foundFiles)

	duration := time.Since(start)

	fmt.Printf("\nTotal bytes read via HTTP: %d\n", remoteiso.TotalBytesRead)
	fmt.Printf("Duration: %v\n", duration)

	err = run("/sbin/gpart", "show")
	if err != nil {
		fmt.Printf("Error executing /sbin/gpart: %v\n", err)
		return
	}

	err = run("/sbin/gpart", "create", "-s", "gpt", "vtbd1")
	if err != nil {
		fmt.Printf("Error creating GPT partition scheme: %v\n", err)
	}

	err = run("/sbin/gpart", "add", "-t", "freebsd-ufs", "vtbd1")
	if err != nil {
		fmt.Printf("Error adding freebsd-ufs partition: %v\n", err)
	}

	err = run("/sbin/newfs", "-U", "/dev/vtbd1p1")
	if err != nil {
		fmt.Printf("Error creating filesystem: %v\n", err)
		return
	}

	err = os.MkdirAll("/mnt/ufs", 0755)
	if err != nil && !os.IsExist(err) {
		fmt.Printf("Error creating /mnt/ufs: %v\n", err)
		return
	}

	err = mount.Mount("/dev/vtbd1p1", "/mnt/ufs", "ufs", "")
	if err != nil {
		fmt.Printf("Error mounting /dev/vtbd1p1 to /mnt/ufs: %v\n", err)
	}

	err = run("/bin/cp", "-avx", "/", "/mnt/ufs")
	if err != nil {
		fmt.Printf("Error copying files to /mnt/ufs: %v\n", err)
		return
	}

	err = run("/sbin/umount", "/mnt/ufs")
	if err != nil {
		fmt.Printf("Error unmounting /mnt/ufs: %v\n", err)
	}
	fmt.Println("bootstrap completed successfully")
}

func run(command string, args ...string) error {
	cmd := exec.Command(command, args...)
	cmd.Stdout = os.Stdout
	cmd.Stderr = os.Stderr
	return cmd.Run()
}

type downloader struct {
	targetDir     string
	remoteRoot    *iso9660.File
	finishedFiles map[string]struct{}
}

func newDownloader(targetDir string, remoteRoot *iso9660.File) *downloader {
	return &downloader{
		targetDir:     targetDir,
		remoteRoot:    remoteRoot,
		finishedFiles: make(map[string]struct{}),
	}
}

func (d *downloader) downloadWithDependencies(remoteFiles []*remoteiso.FileEntry) {
	libraryDeps := map[string]struct{}{}
	for _, entry := range remoteFiles {
		// fmt.Printf(" - %s (size: %d bytes)\n", entry.Path, entry.File.Size())
		if _, done := d.finishedFiles[entry.Path]; done {
			fmt.Printf("Skipping already downloaded %s\n", entry.Path)
			continue
		}
		localPath, err := entry.Download(d.targetDir)
		if err != nil {
			fmt.Printf("Error downloading %s: %v\n", entry.Path, err)
			continue
		}
		d.finishedFiles[entry.Path] = struct{}{}

		deps := getLibraryDependencies(localPath)
		for _, lib := range deps {
			libraryDeps[lib] = struct{}{}
		}
	}

	possibleLibraryPaths := []string{}
	for prefix := range LibraryBaseDirs {
		for lib := range libraryDeps {
			possibleLibraryPaths = append(possibleLibraryPaths, filepath.Join(LibraryBaseDirs[prefix], lib))
		}
	}

	foundLibraries := remoteiso.FindFiles(d.remoteRoot, possibleLibraryPaths)
	if len(foundLibraries) > 0 {
		d.downloadWithDependencies(foundLibraries)
	}
}

func getLibraryDependencies(filePath string) []string {
	f, err := elf.Open(filePath)
	if err != nil {
		var fmtErr *elf.FormatError
		if !errors.As(err, &fmtErr) {
			fmt.Printf("   Cannot scan file %s for dependencies: %v\n", filePath, err)
		}
		return nil
	}
	defer f.Close()

	libs, _ := f.ImportedLibraries()

	return libs
}

func copyInitBinary(targetDir string) {
	// Copy /init-freebsd to targetDir/init-freebsd
	srcPath := "/init-freebsd"
	dstPath := filepath.Join(targetDir, "init-freebsd")

	srcFile, err := os.Open(srcPath)
	if err != nil {
		fmt.Printf("Failed to open source file %s: %v\n", srcPath, err)
		return
	}
	defer srcFile.Close()

	dstFile, err := os.Create(dstPath)
	if err != nil {
		fmt.Printf("Failed to create destination file %s: %v\n", dstPath, err)
		return
	}
	defer dstFile.Close()

	srcInfo, err := srcFile.Stat()
	if err != nil {
		fmt.Printf("Failed to get source file info: %v\n", err)
		return
	}

	_, err = srcFile.WriteTo(dstFile)
	if err != nil {
		fmt.Printf("Failed to copy file content: %v\n", err)
		return
	}

	err = dstFile.Chmod(srcInfo.Mode())
	if err != nil {
		fmt.Printf("Failed to set file permissions: %v\n", err)
		return
	}

	fmt.Printf("Copied %s to %s\n", srcPath, dstPath)
}

func initNetwork() error {
	err := run("/sbin/ifconfig", "vtnet0", "inet", "192.168.127.2/24")
	if err != nil {
		return fmt.Errorf("failed to configure network interface: %w", err)
	}

	err = run("/sbin/route", "add", "default", "192.168.127.1")
	if err != nil {
		return fmt.Errorf("failed to add default route: %w", err)
	}

	return nil
}

func createResolvConf(targetDir string) error {
	resolvPath := filepath.Join(targetDir, "etc", "resolv.conf")
	err := os.MkdirAll(filepath.Dir(resolvPath), 0755)
	if err != nil {
		return fmt.Errorf("failed to create etc directory: %w", err)
	}

	content := "nameserver 192.168.127.1\n"
	err = os.WriteFile(resolvPath, []byte(content), 0644)
	if err != nil {
		return fmt.Errorf("failed to write resolv.conf: %w", err)
	}
	return nil
}
