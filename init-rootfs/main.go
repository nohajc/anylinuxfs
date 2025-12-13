package main

import (
	"context"
	"crypto/x509"
	_ "embed"
	"encoding/pem"
	"flag"
	"fmt"
	"io"
	"net/http"
	"os"
	"os/exec"
	"os/user"
	"path/filepath"
	"strings"
	"time"

	"anylinuxfs/init-rootfs/vmrunner"

	"github.com/BurntSushi/toml"
	"github.com/opencontainers/runtime-spec/specs-go"
	"github.com/opencontainers/umoci"
	"github.com/opencontainers/umoci/oci/cas/dir"
	"github.com/opencontainers/umoci/oci/casext"
	"github.com/opencontainers/umoci/oci/layer"
	"github.com/opencontainers/umoci/pkg/idtools"
	"go.podman.io/image/v5/copy"
	"go.podman.io/image/v5/docker"
	"go.podman.io/image/v5/oci/layout"
	"go.podman.io/image/v5/signature"
	"go.podman.io/image/v5/types"
)

const DEFAULT_DNS_SERVER = "1.1.1.1"

type Config struct {
	ImageName         string
	ImageBasePath     string
	ImageOciPath      string
	Tag               string
	RootfsPath        string
	VmSetupScriptPath string
	PrefixDir         string
	UserStore         string
}

type Preferences struct {
	Alpine AlpineConfig `toml:"alpine"`
}

type AlpineConfig struct {
	CustomPackages []string `toml:"custom_packages"`
}

func defaultConfig(userHomeDir, execDir string) Config {
	imageName := "alpine"
	tag := "latest"

	userStore := filepath.Join(userHomeDir, ".anylinuxfs")
	imageBasePath := filepath.Join(userStore, imageName)
	imageOciPath := filepath.Join(imageBasePath, "oci")
	rootfsPath := filepath.Join(imageBasePath, "rootfs")

	vmSetupScriptPath := "/usr/local/bin/vm-setup.sh"

	prefixDir := filepath.Dir(execDir)

	fmt.Printf("User store: %s\n", userStore)
	fmt.Printf("Image base path: %s\n", imageBasePath)
	fmt.Printf("Image OCI path: %s\n", imageOciPath)
	fmt.Printf("Rootfs path: %s\n", rootfsPath)
	fmt.Printf("Prefix directory: %s\n", prefixDir)

	return Config{
		ImageName:         imageName,
		ImageBasePath:     imageBasePath,
		ImageOciPath:      imageOciPath,
		Tag:               tag,
		RootfsPath:        rootfsPath,
		VmSetupScriptPath: vmSetupScriptPath,
		PrefixDir:         prefixDir,
		UserStore:         userStore,
	}
}

func downloadImage(cfg *Config) error {
	// Define source and destination
	srcRef, err := docker.ParseReference(fmt.Sprintf("//%s:%s", cfg.ImageName, cfg.Tag))
	if err != nil {
		fmt.Println("Error parsing source reference:", err)
		return err
	}

	err = os.MkdirAll(cfg.ImageBasePath, 0755)
	if err != nil {
		fmt.Println("Error creating bundle directory:", err)
		return err
	}

	destRef, err := layout.ParseReference(fmt.Sprintf("%s:%s", cfg.ImageOciPath, cfg.Tag))
	if err != nil {
		fmt.Println("Error parsing destination reference:", err)
		return err
	}

	policy := &signature.Policy{
		Default: []signature.PolicyRequirement{
			signature.NewPRInsecureAcceptAnything(),
		},
	}
	policyCtx, err := signature.NewPolicyContext(policy)
	if err != nil {
		fmt.Println("Error creating policy context:", err)
		return err
	}
	defer policyCtx.Destroy()

	ctx := context.Background()
	ctx, cancel := context.WithTimeout(ctx, 30*time.Second)
	defer cancel()

	// Download image
	_, err = copy.Image(ctx, policyCtx, destRef, srcRef, &copy.Options{
		ReportWriter: os.Stdout,
		SourceCtx: &types.SystemContext{
			OSChoice: "linux",
		},
	})
	if err != nil {
		fmt.Println("Error copying image:", err)
		return err
	}
	return nil
}

func unpackImage(cfg *Config) error {
	engine, err := dir.Open(cfg.ImageOciPath)
	if err != nil {
		fmt.Printf("Error opening image: %v\n", err)
		return err
	}

	engineExt := casext.NewEngine(engine)
	defer engine.Close()

	uidMap, err := idtools.ParseMapping(fmt.Sprintf("0:%d:1", os.Geteuid()))
	if err != nil {
		fmt.Printf("Error parsing UID mapping: %v\n", err)
		return err
	}

	gidMap, err := idtools.ParseMapping(fmt.Sprintf("0:%d:1", os.Getegid()))
	if err != nil {
		fmt.Printf("Error parsing GID mapping: %v\n", err)
		return err
	}

	err = umoci.Unpack(engineExt, cfg.Tag, cfg.ImageBasePath, layer.UnpackOptions{
		MapOptions: layer.MapOptions{
			UIDMappings: []specs.LinuxIDMapping{uidMap},
			GIDMappings: []specs.LinuxIDMapping{gidMap},
			Rootless:    true,
		},
	})
	if err != nil {
		fmt.Printf("Error unpacking image: %v\n", err)
		return err
	}

	currentTime := time.Now()
	_ = os.Chtimes(cfg.RootfsPath, currentTime, currentTime)

	return nil
}

func configureDNS(rootfsPath, nameserver string) error {
	resolvConfPath := fmt.Sprintf("%s/etc/resolv.conf", rootfsPath)

	if nameserver == "" {
		// Fallback default if somehow empty
		nameserver = DEFAULT_DNS_SERVER
	}

	resolvConfContent := fmt.Sprintf("nameserver %s\n", nameserver)
	err := os.WriteFile(resolvConfPath, []byte(resolvConfContent), 0644)
	if err != nil {
		fmt.Printf("Error writing to resolv.conf: %v\n", err)
		return err
	}

	return nil
}

func appendCaCerts(cfg *Config) error {
	userCaCertPath := filepath.Join(cfg.UserStore, "ca-certificates.crt")
	caCertPath := fmt.Sprintf("%s/etc/ssl/certs/ca-certificates.crt", cfg.RootfsPath)

	certs, err := os.ReadFile(userCaCertPath)

	// no certificates to append
	if err != nil || len(certs) == 0 {
		fmt.Printf("Did not find certificates at %s\n", userCaCertPath)
		return nil
	}

	f, err := os.OpenFile(caCertPath, os.O_APPEND|os.O_WRONLY|os.O_CREATE, 0644)
	if err != nil {
		return err
	}

	defer f.Close()

	certCount := 0
	for block, rest := pem.Decode(certs); block != nil; block, rest = pem.Decode(rest) {
		switch block.Type {
		case "CERTIFICATE":
			_, err := x509.ParseCertificate(block.Bytes)
			if err != nil {
				fmt.Printf("Encountered error while parsing CA certificate. Skipping...\n")
				continue
			}

			err = pem.Encode(f, block)
			if err != nil {
				fmt.Printf("Encountered error while writing CA certificate to %s. Skipping...\n", caCertPath)
				continue
			}

			certCount += 1

		case "PRIVATE KEY":
			fmt.Printf("CA certificates cannot contain private keys. Skipping...\n")

		default:
			fmt.Printf("Malformed CA certificate. Skipping...\n")
		}
	}

	fmt.Printf("Added %v entries to CA certificate store\n", certCount)
	return nil
}

func configureFstab(rootfsPath string) error {
	nfsDirs := []string{
		"/var/lib/nfs/rpc_pipefs",
		"/var/lib/nfs/v4recovery",
	}

	for _, dir := range nfsDirs {
		err := os.MkdirAll(fmt.Sprintf("%s%s", rootfsPath, dir), 0755)
		if err != nil {
			fmt.Printf("Error creating directory %s: %v\n", dir, err)
			return err
		}
	}

	fstabPath := fmt.Sprintf("%s/etc/fstab", rootfsPath)
	fstabContent := `rpc_pipefs  /var/lib/nfs/rpc_pipefs  rpc_pipefs  defaults  0  0
nfsd        /proc/fs/nfsd            nfsd        defaults  0  0
`

	err := os.WriteFile(fstabPath, []byte(fstabContent), 0644)
	if err != nil {
		fmt.Printf("Error writing to fstab: %v\n", err)
		return err
	}

	return nil
}

//go:embed default-alpine-packages.txt
var defaultPackagesData string

func getDefaultPackages() []string {
	lines := strings.Split(strings.TrimSpace(defaultPackagesData), "\n")
	packages := make([]string, 0, len(lines))
	for _, line := range lines {
		if trimmed := strings.TrimSpace(line); trimmed != "" {
			packages = append(packages, trimmed)
		}
	}
	return packages
}

func loadCustomPackages(userStore string) []string {
	configPath := filepath.Join(userStore, "config.toml")

	// Check if config file exists
	if _, err := os.Stat(configPath); os.IsNotExist(err) {
		fmt.Printf("Config file not found at %s, using default packages only\n", configPath)
		return []string{}
	}

	var preferences Preferences
	if _, err := toml.DecodeFile(configPath, &preferences); err != nil {
		fmt.Printf("Error reading config file %s: %v, using default packages only\n", configPath, err)
		return []string{}
	}

	fmt.Printf("Loaded %d custom packages from config\n", len(preferences.Alpine.CustomPackages))
	return preferences.Alpine.CustomPackages
}

func writeSetupScript(cfg *Config) error {
	// Load custom packages from config
	customPackages := loadCustomPackages(cfg.UserStore)

	// Default packages
	defaultPackages := getDefaultPackages()

	// Combine default and custom packages
	allPackages := append(defaultPackages, customPackages...)
	packagesStr := strings.Join(allPackages, " ")

	vmSetupScriptPath := fmt.Sprintf("%s%s", cfg.RootfsPath, cfg.VmSetupScriptPath)
	vmSetupScriptContent := fmt.Sprintf(`#!/bin/sh

apk --update --no-cache add %s
MOD_PATH="modules/$(uname -r)"
cd /lib
mkdir -p $MOD_PATH
unsquashfs -d $MOD_PATH modules.squashfs
rm modules.squashfs
depmod -a
rm -v /etc/idmapd.conf /etc/exports
`, packagesStr)

	err := os.WriteFile(vmSetupScriptPath, []byte(vmSetupScriptContent), 0755)
	if err != nil {
		fmt.Printf("Error writing vm-setup.sh: %v\n", err)
		return err
	}

	fmt.Printf("Generated setup script with %d packages (%d default + %d custom)\n",
		len(allPackages), len(defaultPackages), len(customPackages))

	return nil
}

func downloadEntrypointScript(rootfsPath string) error {
	entrypointScriptURL := "https://raw.githubusercontent.com/nohajc/docker-nfs-server/refs/heads/develop/entrypoint.sh"
	entrypointScriptPath := fmt.Sprintf("%s/usr/local/bin/entrypoint.sh", rootfsPath)

	entrypointScriptFile, err := os.OpenFile(entrypointScriptPath, os.O_CREATE|os.O_WRONLY|os.O_TRUNC, 0755)
	if err != nil {
		fmt.Printf("Error creating entrypoint.sh: %v\n", err)
		return err
	}
	defer entrypointScriptFile.Close()

	resp, err := http.Get(entrypointScriptURL)
	if err != nil {
		fmt.Printf("Error downloading entrypoint.sh: %v\n", err)
		return err
	}
	defer resp.Body.Close()

	if resp.StatusCode != http.StatusOK {
		fmt.Printf("Failed to download entrypoint.sh, status code: %d\n", resp.StatusCode)
		return err
	}

	_, err = io.Copy(entrypointScriptFile, resp.Body)
	if err != nil {
		fmt.Printf("Error saving entrypoint.sh: %v\n", err)
		return err
	}

	return nil
}

func copyFile(srcPath, dstPath string) error {
	copyCmd := exec.Command("cp", "-v", srcPath, dstPath)
	copyCmd.Stdout = os.Stdout
	copyCmd.Stderr = os.Stderr

	return copyCmd.Run()
}

func copyVmproxyBinary(prefixDir, rootfsPath string) error {
	vmproxyBin := "vmproxy"
	vmproxySrcPath := filepath.Join(prefixDir, "libexec", vmproxyBin)
	vmproxyDstPath := filepath.Join(rootfsPath, vmproxyBin)

	err := copyFile(vmproxySrcPath, vmproxyDstPath)
	if err != nil {
		fmt.Printf("Error copying vmproxy: %v\n", err)
		return err
	}

	return nil
}

func copyLinuxModules(prefixDir, rootfsPath string) error {
	modulesSquashfs := "modules.squashfs"
	modulesSrcPath := filepath.Join(prefixDir, "lib", modulesSquashfs)
	modulesDstPath := filepath.Join(rootfsPath, "lib", modulesSquashfs)

	err := copyFile(modulesSrcPath, modulesDstPath)
	if err != nil {
		fmt.Printf("Error copying vmproxy: %v\n", err)
		return err
	}

	return nil
}

func initRootfs(cfg *Config, nameserver string) error {
	if _, err := os.Stat(cfg.ImageBasePath); err == nil {
		err = os.RemoveAll(cfg.ImageBasePath)
		if err != nil {
			fmt.Printf("Error removing existing directory %s: %v\n", cfg.ImageBasePath, err)
			return err
		}
	}

	if err := downloadImage(cfg); err != nil {
		return err
	}

	if err := unpackImage(cfg); err != nil {
		return err
	}

	if err := configureDNS(cfg.RootfsPath, nameserver); err != nil {
		return err
	}

	if err := appendCaCerts(cfg); err != nil {
		return err
	}

	if err := configureFstab(cfg.RootfsPath); err != nil {
		return err
	}

	if err := writeSetupScript(cfg); err != nil {
		return err
	}

	if err := downloadEntrypointScript(cfg.RootfsPath); err != nil {
		return err
	}

	if err := copyLinuxModules(cfg.PrefixDir, cfg.RootfsPath); err != nil {
		return err
	}

	return copyVmproxyBinary(cfg.PrefixDir, cfg.RootfsPath)
}

func resolveExecDir() (string, error) {
	execPath, err := os.Executable()
	if err != nil {
		fmt.Printf("Error getting executable path: %v\n", err)
		return "", err
	}
	execPath, err = filepath.EvalSymlinks(execPath)
	if err != nil {
		fmt.Printf("Error resolving symlinks: %v\n", err)
		return "", err
	}
	return filepath.Dir(execPath), nil
}

func main() {
	var nameserver string
	flag.StringVar(&nameserver, "n", DEFAULT_DNS_SERVER, "Nameserver IP to write into /etc/resolv.conf")
	flag.Parse()

	execDir, err := resolveExecDir()
	if err != nil {
		fmt.Printf("Error resolving exec dir: %v\n", err)
		os.Exit(1)
	}
	currentUser, err := user.Current()
	if err != nil {
		fmt.Printf("Error getting current user: %v\n", err)
		os.Exit(1)
	}
	if currentUser.HomeDir == "" {
		fmt.Println("Current user does not have a home directory.")
		os.Exit(1)
	}
	cfg := defaultConfig(currentUser.HomeDir, execDir)

	err = initRootfs(&cfg, nameserver)
	if err != nil {
		os.Exit(1)
	}

	kernelPath := filepath.Join(cfg.PrefixDir, "libexec", "Image")
	err = vmrunner.Run(kernelPath, cfg.RootfsPath, cfg.VmSetupScriptPath)
	if err != nil {
		fmt.Printf("Failed to run VM: %v\n", err)
		os.Exit(1)
	}
}
