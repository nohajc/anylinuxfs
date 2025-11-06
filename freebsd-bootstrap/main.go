package main

import (
	"anylinuxfs/freebsd-bootstrap/chroot"
	"anylinuxfs/freebsd-bootstrap/mount"
	"anylinuxfs/freebsd-bootstrap/remoteiso"
	"debug/elf"
	"fmt"
	"io/fs"
	"net/http"
	"os"
	"os/exec"
	"path/filepath"
	"time"

	"github.com/kdomanski/iso9660"
)

var RequiredFiles = []string{
	"/libexec/ld-elf.so.1", "/sbin/gpart", "/sbin/newfs", "/usr/sbin/nfsd",
	"/lib/geom/geom_part.so",
}
var LibraryBaseDirs = []string{"/lib", "/usr/lib"}

func mountTarget() error {
	// just testing if mount works
	return mount.Mount("/dev/gpt/efiesp", "/mnt/efi", "msdosfs", "")
	// return mount.Mount("/dev/vtbd0p1", "/mnt", "ufs", "")
}

func main() {
	workdir := "tmp"
	err := os.Mkdir(workdir, 0755)
	if err != nil && !os.IsExist(err) {
		panic(err)
	}
	err = mount.Mount("tmpfs", workdir, "tmpfs", "")
	if err != nil {
		panic(err)
	}

	url := "https://download.freebsd.org/releases/ISO-IMAGES/14.3/FreeBSD-14.3-RELEASE-arm64-aarch64-bootonly.iso"

	reader := &remoteiso.HTTPReaderAt{
		URL:    url,
		Client: &http.Client{},
	}

	cached := &remoteiso.CachedReaderAt{
		Base:      reader,
		BlockSize: 128 * 1024,
		Cache:     make(map[int64][]byte),
	}

	image, err := iso9660.OpenImage(cached)
	if err != nil {
		panic(err)
	}

	root, err := image.RootDir()
	if err != nil {
		panic(err)
	}

	fmt.Printf("Reading %s:\n\n", url)

	start := time.Now()
	// listDir(root, "")

	foundFiles := remoteiso.FindFiles(root, RequiredFiles)
	d := newDownloader(workdir, root)
	d.downloadWithDependencies(foundFiles)

	duration := time.Since(start)

	fmt.Printf("\nTotal bytes read via HTTP: %d\n", remoteiso.TotalBytesRead)
	fmt.Printf("Duration: %v\n", duration)

	// Switch to a temporary root populated from the ISO
	err = os.Chdir(workdir)
	if err != nil {
		panic(err)
	}
	err = chroot.Chroot(".")
	if err != nil {
		panic(err)
	}

	err = os.Mkdir("/dev", 0755)
	if err != nil && !os.IsExist(err) {
		panic(err)
	}
	err = mount.Mount("devfs", "/dev", "devfs", "")
	if err != nil {
		panic(err)
	}

	fmt.Printf("\nListing / after chroot:\n")
	err = filepath.WalkDir(".", func(path string, d fs.DirEntry, err error) error {
		if err != nil {
			return err
		}
		if d.IsDir() {
			fmt.Printf("DIR:  %s\n", path)
		} else {
			info, _ := d.Info()
			fmt.Printf("FILE: %s (%d bytes)\n", path, info.Size())
		}
		return nil
	})
	if err != nil {
		fmt.Printf("Error walking directory: %v\n", err)
	}

	// Run /sbin/gpart show
	fmt.Printf("\nExecuting /sbin/gpart show:\n")
	cmd := exec.Command("/sbin/gpart", "show")
	cmd.Stdout = os.Stdout
	cmd.Stderr = os.Stderr
	err = cmd.Run()
	if err != nil {
		fmt.Printf("Error executing /sbin/gpart: %v\n", err)
	}
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
	fmt.Printf("Found files:\n")
	for _, entry := range remoteFiles {
		fmt.Printf(" - %s (size: %d bytes)\n", entry.Path, entry.File.Size())
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
		fmt.Printf("   Error opening ELF file %s: %v\n", filePath, err)
		return nil
	}
	defer f.Close()

	libs, _ := f.ImportedLibraries()

	return libs
}
