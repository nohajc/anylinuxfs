package main

import (
	"anylinuxfs/freebsd-bootstrap/chroot"
	"anylinuxfs/freebsd-bootstrap/mount"
	"anylinuxfs/freebsd-bootstrap/oci"
	"anylinuxfs/freebsd-bootstrap/remoteiso"
	"debug/elf"
	_ "embed"
	"encoding/json"
	"errors"
	"fmt"
	"maps"
	"net/http"
	"os"
	"os/exec"
	"path/filepath"
	"slices"
	"strings"
	"time"

	"github.com/kdomanski/iso9660"
)

type Config struct {
	ISOURL string `json:"iso_url"`
}

func loadConfig(path string) (Config, error) {
	f, err := os.Open(path)
	if err != nil {
		return Config{}, fmt.Errorf("open config: %w", err)
	}
	defer f.Close()

	dec := json.NewDecoder(f)
	var c Config
	if err := dec.Decode(&c); err != nil {
		return Config{}, fmt.Errorf("decode config: %w", err)
	}
	if c.ISOURL == "" {
		return Config{}, fmt.Errorf("config iso_url is empty")
	}
	return c, nil
}

// TODO: include custom files specified by user?
var RequiredFiles = []string{
	"/lib/geom/geom_part.so",
	"/sbin/fsck_ffs",
	"/sbin/fsck_ufs",
	"/sbin/gpart",
	"/sbin/newfs",
	"/sbin/zfs",
	"/sbin/zpool",
	"/usr/bin/ee",
	"/usr/bin/file",
	"/usr/bin/ldd",
	"/usr/bin/rpcinfo",
	"/usr/bin/showmount",
	"/usr/bin/which",
	"/usr/lib/pam_xdg.so",
	"/usr/sbin/mountd",
	"/usr/sbin/nfsd",
	"/usr/sbin/rpcbind",
	"/usr/sbin/rpc.statd",
	"/usr/sbin/rpc.lockd",
}
var LibraryBaseDirs = []string{"/lib", "/usr/lib"}

func main() {
	fmt.Println("Bootstrap started")

	// Load ISO URL from config.json before performing operations
	config, err := loadConfig("config.json")
	freebsdISO := config.ISOURL
	if err != nil {
		fmt.Printf("Warning: could not load config.json (%v).\n", err)
		return
	}

	workdir := "tmp"
	if _, err := os.Stat(workdir); os.IsNotExist(err) {
		err := os.Mkdir(workdir, 0755)
		if err != nil {
			fmt.Printf("Failed to create workdir %s: %v\n", workdir, err)
			return
		}
	}
	err = mount.Mount("tmpfs", workdir, "tmpfs", "")
	if err != nil {
		fmt.Printf("Failed to mount tmpfs on %s: %v\n", workdir, err)
		return
	}
	fmt.Println("mounted tmpfs")

	err = copyInitBinary(workdir)
	if err != nil {
		fmt.Printf("Failed to copy init binary: %v\n", err)
		return
	}

	err = copyNFSLauncher(workdir)
	if err != nil {
		fmt.Printf("Failed to copy NFS launcher: %v\n", err)
		return
	}

	kernelDir := filepath.Join(workdir, "boot", "kernel")
	err = os.MkdirAll(kernelDir, 0755)
	if err != nil {
		fmt.Printf("Failed to create kernel directory %s: %v\n", kernelDir, err)
		return
	}
	err = copyKernelModules(kernelDir)
	if err != nil {
		fmt.Printf("%v\n", err)
		return
	}

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
	err = oci.Unpack(ociDir, ".")
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

	err = createFstab("/")
	if err != nil {
		fmt.Printf("Error creating fstab: %v\n", err)
		return
	}
	fmt.Println("created fstab")

	err = editGettytab("/")
	if err != nil {
		fmt.Printf("Error editing gettytab: %v\n", err)
		return
	}
	fmt.Println("edited gettytab")

	err = createScripts("/")
	if err != nil {
		fmt.Printf("Error creating scripts: %v\n", err)
		return
	}
	fmt.Println("created scripts")

	reader := &remoteiso.HTTPReaderAt{
		URL:    freebsdISO,
		Client: &http.Client{},
	}

	cached := &remoteiso.CachedReaderAt{
		Base:      reader,
		BlockSize: 128 * 1024,
		Cache:     make(map[int64][]byte),
	}

	image, err := iso9660.OpenImage(cached)
	if err != nil {
		fmt.Printf("Failed to open ISO image %s: %v\n", freebsdISO, err)
		return
	}

	root, err := image.RootDir()
	if err != nil {
		fmt.Printf("Failed to get root directory of ISO: %v\n", err)
		return
	}

	fmt.Printf("Reading %s:\n", freebsdISO)

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

	err = run("/sbin/gpart", "add", "-t", "freebsd-ufs", "-l", "rootfs", "vtbd1")
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
	pathDeps := map[string]struct{}{}
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

		deps := getDependencies(localPath)
		for _, d := range deps {
			if strings.HasPrefix(d, "/") {
				pathDeps[d] = struct{}{}
			} else {
				libraryDeps[d] = struct{}{}
			}
		}
	}

	possiblePaths := []string{}
	for prefix := range LibraryBaseDirs {
		for lib := range libraryDeps {
			possiblePaths = append(possiblePaths, filepath.Join(LibraryBaseDirs[prefix], lib))
		}
	}
	possiblePaths = append(possiblePaths, slices.Collect(maps.Keys(pathDeps))...)

	foundLibraries := remoteiso.FindFiles(d.remoteRoot, possiblePaths)
	if len(foundLibraries) > 0 {
		d.downloadWithDependencies(foundLibraries)
	}
}

func getDependencies(filePath string) []string {
	// Check if the file is a symlink and return its target if so
	fileInfo, err := os.Lstat(filePath)
	if err != nil {
		return nil
	}
	if fileInfo.Mode()&os.ModeSymlink != 0 {
		target, err := os.Readlink(filePath)
		if err != nil {
			fmt.Printf("   Cannot resolve symlink %s: %v\n", filePath, err)
			return nil
		}
		if !strings.HasPrefix(target, "/") {
			target = filepath.Clean(filepath.Join(filepath.Dir(filePath), target))
		}
		// fmt.Printf("   Adding dependency: %s\n", target)
		return []string{target}
	}
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

func copyFile(srcPath, dstPath string) error {
	srcFile, err := os.Open(srcPath)
	if err != nil {
		return fmt.Errorf("failed to open source file %s: %w", srcPath, err)
	}
	defer srcFile.Close()

	dstFile, err := os.Create(dstPath)
	if err != nil {
		return fmt.Errorf("failed to create destination file %s: %w", dstPath, err)
	}
	defer dstFile.Close()

	srcInfo, err := srcFile.Stat()
	if err != nil {
		return fmt.Errorf("failed to get source file info: %w", err)
	}

	_, err = srcFile.WriteTo(dstFile)
	if err != nil {
		return fmt.Errorf("failed to copy file content: %w", err)
	}

	err = dstFile.Chmod(srcInfo.Mode())
	if err != nil {
		return fmt.Errorf("failed to set file permissions: %w", err)
	}

	fmt.Printf("Copied %s to %s\n", srcPath, dstPath)
	return nil
}

func copyInitBinary(targetDir string) error {
	srcPath := "/init-freebsd"
	dstPath := filepath.Join(targetDir, "init-freebsd")

	return copyFile(srcPath, dstPath)
}

func copyNFSLauncher(targetDir string) error {
	srcPath := "/entrypoint.sh"
	dstDir := filepath.Join(targetDir, "usr", "local", "bin")
	err := os.MkdirAll(dstDir, 0755)
	if err != nil {
		return fmt.Errorf("failed to create directory for NFS launcher: %w", err)
	}
	dstFile := filepath.Join(dstDir, "entrypoint.sh")

	return copyFile(srcPath, dstFile)
}

func copyKernelModules(targetDir string) error {
	files, err := filepath.Glob("/*.ko")
	if err != nil {
		return fmt.Errorf("invalid glob pattern: %w", err)
	}

	for _, srcPath := range files {
		dstPath := filepath.Join(targetDir, filepath.Base(srcPath))

		err := copyFile(srcPath, dstPath)
		if err != nil {
			return fmt.Errorf("Failed to copy kernel module %s: %w", srcPath, err)
		}
	}
	return nil
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

func createFstab(targetDir string) error {
	fstabPath := filepath.Join(targetDir, "etc", "fstab")
	err := os.MkdirAll(filepath.Dir(fstabPath), 0755)
	if err != nil {
		return fmt.Errorf("failed to create etc directory: %w", err)
	}

	content := "/dev/gpt/rootfs   /       ufs   rw      1       1\n"
	err = os.WriteFile(fstabPath, []byte(content), 0644)
	if err != nil {
		return fmt.Errorf("failed to write fstab: %w", err)
	}
	return nil
}

func editGettytab(baseDir string) error {
	gettytabPath := filepath.Join(baseDir, "etc", "gettytab")

	file, err := os.OpenFile(gettytabPath, os.O_WRONLY|os.O_APPEND|os.O_CREATE, 0644)
	if err != nil {
		return fmt.Errorf("failed to open gettytab file: %w", err)
	}
	defer file.Close()

	content := "\nal.3wire:\\\n\t:al=root:np:nc:sp#0:\n"
	_, err = file.WriteString(content)
	if err != nil {
		return fmt.Errorf("failed to write to gettytab: %w", err)
	}

	return nil
}

const InitNetworkScript = `#!/bin/sh

ifconfig vtnet0 inet 192.168.127.2/24
route add default 192.168.127.1
`

const StartShellScript = `#!/bin/sh

trap "mount -fr /" EXIT; mount -u / && TERM=vt100 /usr/libexec/getty al.3wire
`

var AllScripts = map[string]string{
	"init-network.sh": InitNetworkScript,
	"start-shell.sh":  StartShellScript,
}

func createScripts(targetDir string) error {
	for name, content := range AllScripts {
		scriptPath := filepath.Join(targetDir, name)
		err := os.WriteFile(scriptPath, []byte(content), 0755)
		if err != nil {
			return fmt.Errorf("failed to create script %s: %w", scriptPath, err)
		}
	}
	return nil
}
