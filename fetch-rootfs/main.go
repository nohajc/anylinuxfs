package main

import (
	"context"
	"fmt"
	"os"
	"time"

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

func main() {
	ctx := context.Background()

	imageName := "alpine"
	imagePath := fmt.Sprintf("%s/oci", imageName)
	tag := "latest"

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

	rootfsPath := fmt.Sprintf("%s/rootfs", imageName)
	currentTime := time.Now()
	_ = os.Chtimes(rootfsPath, currentTime, currentTime)
}
