package oci

import (
	"context"
	"errors"
	"fmt"

	"github.com/apex/log"
	ispec "github.com/opencontainers/image-spec/specs-go/v1"
	"github.com/opencontainers/umoci"
	"github.com/opencontainers/umoci/oci/cas/dir"
	"github.com/opencontainers/umoci/oci/casext"
	"github.com/opencontainers/umoci/oci/layer"
)

func Unpack(imagePath, rootfsPath string) error {
	var unpackOptions layer.UnpackOptions
	var meta umoci.Meta

	unpackOptions.KeepDirlinks = true

	// Get a reference to the CAS.
	engine, err := dir.Open(imagePath)
	if err != nil {
		return fmt.Errorf("open CAS: %w", err)
	}
	engineExt := casext.NewEngine(engine)
	defer engine.Close()

	names, err := engineExt.ListReferences(context.Background())
	if err != nil {
		return fmt.Errorf("list references: %w", err)
	}
	if len(names) == 0 {
		return errors.New("no image tags found in the specified OCI image")
	}

	fromName := names[0]
	fromDescriptorPaths, err := engineExt.ResolveReference(context.Background(), fromName)
	if err != nil {
		return fmt.Errorf("get descriptor: %w", err)
	}
	if len(fromDescriptorPaths) == 0 {
		return fmt.Errorf("tag is not found: %s", fromName)
	}
	if len(fromDescriptorPaths) != 1 {
		return fmt.Errorf("tag is ambiguous: %s", fromName)
	}
	meta.From = fromDescriptorPaths[0]

	manifestBlob, err := engineExt.FromDescriptor(context.Background(), meta.From.Descriptor())
	if err != nil {
		return fmt.Errorf("get manifest: %w", err)
	}
	defer manifestBlob.Close()

	if manifestBlob.Descriptor.MediaType != ispec.MediaTypeImageManifest {
		return fmt.Errorf("descriptor does not point to ispec.MediaTypeImageManifest: not implemented: %s", manifestBlob.Descriptor.MediaType)
	}

	log.WithFields(log.Fields{
		"image":  imagePath,
		"rootfs": rootfsPath,
		"ref":    fromName,
	}).Debugf("umoci: unpacking OCI image")

	// Get the manifest.
	manifest, ok := manifestBlob.Data.(ispec.Manifest)
	if !ok {
		// Should _never_ be reached.
		return fmt.Errorf("[internal error] unknown manifest blob type: %s", manifestBlob.Descriptor.MediaType)
	}

	log.Infof("unpacking rootfs ...")
	if err := layer.UnpackRootfs(context.Background(), engineExt, rootfsPath, manifest, &unpackOptions); err != nil {
		return fmt.Errorf("create rootfs: %w", err)
	}
	log.Infof("... done")

	log.Infof("unpacked image rootfs: %s", rootfsPath)
	return nil
}
