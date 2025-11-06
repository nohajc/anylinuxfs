package remoteiso

import (
	"fmt"
	"io"
	"net/http"
	"os"
	"path/filepath"
	"strings"

	"github.com/kdomanski/iso9660"
)

// FileEntry wraps an iso9660.File with its absolute path
type FileEntry struct {
	File *iso9660.File
	Path string
}

func (entry FileEntry) Download(baseDir string) (string, error) {
	// Create the full local path
	localPath := filepath.Join(baseDir, entry.Path)
	// fmt.Printf("Downloading %s to %s\n", entry.Path, localPath)

	// Create directory structure
	dir := filepath.Dir(localPath)
	if err := os.MkdirAll(dir, 0755); err != nil {
		return "", fmt.Errorf("failed to create directory %s: %w", dir, err)
	}

	if entry.File.Mode()&os.ModeSymlink != 0 {
		origTarget := entry.File.SymlinkTarget()
		if origTarget == "" {
			return "", fmt.Errorf("symlink target for %s is empty", entry.Path)
		}
		target := origTarget
		if strings.HasPrefix("/", origTarget) {
			target = filepath.Join(baseDir, origTarget)
		}
		if _, err := os.Lstat(localPath); err == nil {
			_ = os.Remove(localPath)
		}
		if err := os.Symlink(target, localPath); err != nil {
			return "", fmt.Errorf("failed to create symlink %s -> %s: %w", localPath, target, err)
		}
		fmt.Printf("Created symlink %s -> %s\n", entry.Path, origTarget)
		return localPath, nil
	}

	// Create the local file (but first remove it to reset permissions too)
	_ = os.Chmod(localPath, entry.File.Mode()|0200) // ensure write permission before deleting
	_ = os.Remove(localPath)
	localFile, err := os.OpenFile(localPath, os.O_CREATE|os.O_WRONLY|os.O_TRUNC, entry.File.Mode())
	if err != nil {
		return "", fmt.Errorf("failed to create file %s: %w", localPath, err)
	}
	defer localFile.Close()

	// Get reader for the ISO file content
	reader := entry.File.Reader()

	// Copy content
	_, err = io.Copy(localFile, reader)
	if err != nil {
		return "", fmt.Errorf("failed to copy content to %s: %w", localPath, err)
	}

	fmt.Printf("Downloaded %s (%d bytes)\n", entry.Path, entry.File.Size())
	return localPath, nil
}

// HTTPReaderAt implements io.ReaderAt backed by HTTP Range requests.
type HTTPReaderAt struct {
	URL    string
	Client *http.Client
}

var TotalBytesRead int64 = 0

// ReadAt reads len(p) bytes starting at offset off.
func (r *HTTPReaderAt) ReadAt(p []byte, off int64) (int, error) {
	// fmt.Printf("HTTP ReadAt: offset=%d, length=%d\n", off, len(p))
	TotalBytesRead += int64(len(p))

	end := off + int64(len(p)) - 1
	req, err := http.NewRequest("GET", r.URL, nil)
	if err != nil {
		return 0, err
	}
	req.Header.Set("Range", fmt.Sprintf("bytes=%d-%d", off, end))

	resp, err := r.Client.Do(req)
	if err != nil {
		return 0, err
	}
	defer resp.Body.Close()

	if resp.StatusCode != http.StatusPartialContent && resp.StatusCode != http.StatusOK {
		return 0, fmt.Errorf("unexpected HTTP status: %s", resp.Status)
	}

	n, err := io.ReadFull(resp.Body, p)
	if err == io.ErrUnexpectedEOF {
		// Allow short reads at EOF
		return n, io.EOF
	}
	return n, err
}

type CachedReaderAt struct {
	Base      *HTTPReaderAt
	BlockSize int64
	Cache     map[int64][]byte // key = block number
}

func (c *CachedReaderAt) ReadAt(p []byte, off int64) (int, error) {
	startBlock := off / c.BlockSize
	endBlock := (off + int64(len(p)) - 1) / c.BlockSize

	var read int
	for blk := startBlock; blk <= endBlock; blk++ {
		blockOff := blk * c.BlockSize
		data, ok := c.Cache[blk]
		if !ok {
			buf := make([]byte, c.BlockSize)
			_, err := c.Base.ReadAt(buf, blockOff)
			if err != nil && err != io.EOF {
				return read, err
			}
			c.Cache[blk] = buf
			data = buf
		}
		blockStart := max(off, blockOff)
		blockEnd := min(off+int64(len(p)), blockOff+int64(len(data)))
		copy(p[blockStart-off:blockEnd-off], data[blockStart-blockOff:blockEnd-blockOff])
		read += int(blockEnd - blockStart)
	}
	return read, nil
}

func ListDir(dir *iso9660.File, prefix string) {
	entries, err := dir.GetChildren()
	if err != nil {
		fmt.Printf("%s[error reading dir]: %v\n", prefix, err)
		return
	}
	for _, entry := range entries {
		fmt.Printf("%s%s\n", prefix, entry.Name())
		if entry.IsDir() {
			ListDir(entry, prefix+"  ")
		}
	}
}

func FindFiles(root *iso9660.File, paths []string) []*FileEntry {
	var found []*FileEntry

	for _, targetPath := range paths {
		if file := findFileByPath(root, targetPath); file != nil {
			found = append(found, &FileEntry{
				File: file,
				Path: targetPath,
			})
		}
	}

	return found
}

func findFileByPath(root *iso9660.File, targetPath string) *iso9660.File {
	// Handle root path
	if targetPath == "/" || targetPath == "" {
		return root
	}

	// Split path into components, removing empty strings
	pathParts := []string{}
	for _, part := range splitPath(targetPath) {
		if part != "" {
			pathParts = append(pathParts, part)
		}
	}
	// fmt.Printf("DEBUG pathParts: %v\n", pathParts)

	// Start from root and traverse down
	current := root
	for _, part := range pathParts {
		entries, err := current.GetChildren()
		if err != nil {
			return nil
		}

		// Look for the part in current directory
		var found *iso9660.File
		for _, entry := range entries {
			if entry.Name() == part {
				found = entry
				break
			}
		}

		if found == nil {
			return nil // Path component not found
		}

		current = found
	}

	return current
}

// Simple path splitter (splits on '/' and filters empty strings)
func splitPath(path string) []string {
	var parts []string
	current := ""

	for i, char := range path {
		if char == '/' {
			if current != "" {
				parts = append(parts, current)
				current = ""
			}
		} else {
			current += string(char)
		}

		// Add last part if we're at the end
		if i == len(path)-1 && current != "" {
			parts = append(parts, current)
		}
	}

	return parts
}
