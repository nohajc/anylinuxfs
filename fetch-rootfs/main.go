package main

import (
	"context"
	"fmt"
	"io"
	"net/http"
	"os"
	"time"

	"anylinuxfs/fetch-rootfs/vmrunner"

	"github.com/containers/image/v5/copy"
	"github.com/containers/image/v5/docker"
	"github.com/containers/image/v5/oci/layout"
	"github.com/containers/image/v5/signature"
	"github.com/containers/image/v5/types"
	"github.com/opencontainers/runtime-spec/specs-go"
	"github.com/opencontainers/umoci"
	"github.com/opencontainers/umoci/oci/cas/dir"
	"github.com/opencontainers/umoci/oci/casext"
	"github.com/opencontainers/umoci/oci/layer"
	"github.com/opencontainers/umoci/pkg/idtools"
)

var imageName = "alpine"
var imagePath = fmt.Sprintf("%s/oci", imageName)
var tag = "latest"

var rootfsPath = fmt.Sprintf("%s/rootfs", imageName)
var vmSetupScriptPath = "/usr/local/bin/vm-setup.sh"

func initRootfs() {
	if _, err := os.Stat(imageName); err == nil {
		err = os.RemoveAll(imageName)
		if err != nil {
			fmt.Printf("Error removing existing directory %s: %v\n", imageName, err)
			os.Exit(1)
		}
	}

	// Define source and destination
	srcRef, err := docker.ParseReference(fmt.Sprintf("//%s:%s", imageName, tag))
	if err != nil {
		fmt.Println("Error parsing source reference:", err)
		os.Exit(1)
	}

	err = os.MkdirAll(imageName, 0755)
	if err != nil {
		fmt.Println("Error creating bundle directory:", err)
		os.Exit(1)
	}

	destRef, err := layout.ParseReference(fmt.Sprintf("%s:%s", imagePath, tag))
	if err != nil {
		fmt.Println("Error parsing destination reference:", err)
		os.Exit(1)
	}

	policy := &signature.Policy{
		Default: []signature.PolicyRequirement{
			signature.NewPRInsecureAcceptAnything(),
		},
	}
	policyCtx, err := signature.NewPolicyContext(policy)
	if err != nil {
		fmt.Println("Error creating policy context:", err)
		os.Exit(1)
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
		os.Exit(1)
	}

	engine, err := dir.Open(imagePath)
	if err != nil {
		fmt.Printf("Error opening image: %v\n", err)
		os.Exit(1)
	}

	engineExt := casext.NewEngine(engine)
	defer engine.Close()

	uidMap, err := idtools.ParseMapping(fmt.Sprintf("0:%d:1", os.Geteuid()))
	if err != nil {
		fmt.Printf("Error parsing UID mapping: %v\n", err)
		os.Exit(1)
	}

	gidMap, err := idtools.ParseMapping(fmt.Sprintf("0:%d:1", os.Getegid()))
	if err != nil {
		fmt.Printf("Error parsing GID mapping: %v\n", err)
		os.Exit(1)
	}

	err = umoci.Unpack(engineExt, tag, imageName, layer.UnpackOptions{
		MapOptions: layer.MapOptions{
			UIDMappings: []specs.LinuxIDMapping{uidMap},
			GIDMappings: []specs.LinuxIDMapping{gidMap},
			Rootless:    true,
		},
	})
	if err != nil {
		fmt.Printf("Error unpacking image: %v\n", err)
		os.Exit(1)
	}

	currentTime := time.Now()
	_ = os.Chtimes(rootfsPath, currentTime, currentTime)

	resolvConfPath := fmt.Sprintf("%s/etc/resolv.conf", rootfsPath)

	resolvConfContent := "nameserver 1.1.1.1\n"
	err = os.WriteFile(resolvConfPath, []byte(resolvConfContent), 0644)
	if err != nil {
		fmt.Printf("Error writing to resolv.conf: %v\n", err)
		os.Exit(1)
	}

	nfsDirs := []string{
		"/var/lib/nfs/rpc_pipefs",
		"/var/lib/nfs/v4recovery",
	}

	for _, dir := range nfsDirs {
		err := os.MkdirAll(fmt.Sprintf("%s%s", rootfsPath, dir), 0755)
		if err != nil {
			fmt.Printf("Error creating directory %s: %v\n", dir, err)
			os.Exit(1)
		}
	}

	fstabPath := fmt.Sprintf("%s/etc/fstab", rootfsPath)
	fstabContent := `rpc_pipefs  /var/lib/nfs/rpc_pipefs  rpc_pipefs  defaults  0  0
nfsd        /proc/fs/nfsd            nfsd        defaults  0  0
`

	err = os.WriteFile(fstabPath, []byte(fstabContent), 0644)
	if err != nil {
		fmt.Printf("Error writing to fstab: %v\n", err)
		os.Exit(1)
	}

	vmSetupScriptPath := fmt.Sprintf("%s%s", rootfsPath, vmSetupScriptPath)
	vmSetupScriptContent := `#!/bin/sh

apk --update --no-cache add nfs-utils
rm -v /etc/idmapd.conf /etc/exports
`

	err = os.WriteFile(vmSetupScriptPath, []byte(vmSetupScriptContent), 0755)
	if err != nil {
		fmt.Printf("Error writing vm-setup.sh: %v\n", err)
		os.Exit(1)
	}

	entrypointScriptURL := "https://raw.githubusercontent.com/nohajc/docker-nfs-server/refs/heads/develop/entrypoint.sh"
	entrypointScriptPath := fmt.Sprintf("%s/usr/local/bin/entrypoint.sh", rootfsPath)

	entrypointScriptFile, err := os.OpenFile(entrypointScriptPath, os.O_CREATE|os.O_WRONLY|os.O_TRUNC, 0755)
	if err != nil {
		fmt.Printf("Error creating entrypoint.sh: %v\n", err)
		os.Exit(1)
	}
	defer entrypointScriptFile.Close()

	resp, err := http.Get(entrypointScriptURL)
	if err != nil {
		fmt.Printf("Error downloading entrypoint.sh: %v\n", err)
		os.Exit(1)
	}
	defer resp.Body.Close()

	if resp.StatusCode != http.StatusOK {
		fmt.Printf("Failed to download entrypoint.sh, status code: %d\n", resp.StatusCode)
		os.Exit(1)
	}

	_, err = io.Copy(entrypointScriptFile, resp.Body)
	if err != nil {
		fmt.Printf("Error saving entrypoint.sh: %v\n", err)
		os.Exit(1)
	}
}

func main() {
	initRootfs()

	err := vmrunner.Run(rootfsPath, vmSetupScriptPath)
	if err != nil {
		fmt.Printf("Failed to run VM: %v\n", err)
		os.Exit(1)
	}
}
