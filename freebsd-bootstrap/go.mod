module anylinuxfs/freebsd-bootstrap

go 1.25.4

require (
	github.com/apex/log v1.9.0
	github.com/kdomanski/iso9660 v0.4.0
	github.com/opencontainers/image-spec v1.1.1
	github.com/opencontainers/umoci v0.6.0
	golang.org/x/sys v0.46.0
)

require (
	github.com/AdaLogics/go-fuzz-headers v0.0.0-20230106234847-43070de90fa1 // indirect
	github.com/blang/semver/v4 v4.0.0 // indirect
	github.com/containerd/log v0.1.0 // indirect
	github.com/containerd/platforms v0.2.1 // indirect
	github.com/moby/sys/userns v0.1.0 // indirect
)

require (
	github.com/cpuguy83/go-md2man/v2 v2.0.7 // indirect
	github.com/cyphar/filepath-securejoin v0.7.0 // indirect
	github.com/docker/go-units v0.5.0 // indirect
	github.com/klauspost/compress v1.16.0 // indirect
	github.com/klauspost/pgzip v1.2.6 // indirect
	github.com/moby/sys/user v0.4.0 // indirect
	github.com/opencontainers/go-digest v1.0.0 // indirect
	github.com/opencontainers/runtime-spec v1.3.0 // indirect
	github.com/pkg/errors v0.9.1 // indirect
	github.com/rootless-containers/proto/go-proto v0.0.0-20230421021042-4cd87ebadd67 // indirect
	github.com/russross/blackfriday/v2 v2.1.0 // indirect
	github.com/sirupsen/logrus v1.9.4 // indirect
	github.com/urfave/cli v1.22.17 // indirect
	github.com/vbatts/go-mtree v0.6.1-0.20250911112631-8307d76bc1b9 // indirect
	golang.org/x/crypto v0.52.0 // indirect
	google.golang.org/protobuf v1.36.11 // indirect
)

replace github.com/kdomanski/iso9660 => github.com/nohajc/iso9660 v0.0.0-20251105191846-0bf547744ee1

// replace github.com/kdomanski/iso9660 => ../../../3rd-party/iso9660
