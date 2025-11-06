package main

import (
	"anylinuxfs/freebsd-bootstrap/remoteiso"
	"debug/elf"
	"fmt"
	"net/http"
	"time"

	"github.com/kdomanski/iso9660"
	"github.com/moby/sys/mount"
)

func mountUFS() error {
	return mount.Mount("/dev/vtbd0p1", "/mnt", "ufs", "")
}

func main() {
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

	targetPaths := []string{"/libexec/ld-elf.so.1", "/lib/libufs.so.7", "/sbin/newfs", "/usr/lib/libufs.so", "/usr/sbin/nfsd"}
	foundFiles := remoteiso.FindFiles(root, targetPaths)

	fmt.Printf("Found files:\n")
	for _, entry := range foundFiles {
		fmt.Printf(" - %s (size: %d bytes)\n", entry.Path, entry.File.Size())
		localPath, err := entry.Download(".")
		if err != nil {
			fmt.Printf("   Error downloading %s: %v\n", entry.Path, err)
			continue
		}
		libraryDeps := getLibraryDependencies(localPath)
		fmt.Printf("   Library dependencies for %s:\n", entry.Path)
		for _, lib := range libraryDeps {
			fmt.Printf("     %s\n", lib)
		}
	}
	duration := time.Since(start)

	fmt.Printf("\nTotal bytes read via HTTP: %d\n", remoteiso.TotalBytesRead)
	fmt.Printf("Duration: %v\n", duration)
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
