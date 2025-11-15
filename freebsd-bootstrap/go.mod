module anylinuxfs/freebsd-bootstrap

go 1.25.2

require (
	github.com/apex/log v1.4.0
	github.com/kdomanski/iso9660 v0.4.0
	github.com/opencontainers/image-spec v1.0.1
	github.com/opencontainers/umoci v0.4.7
	golang.org/x/sys v0.31.0
)

require (
	github.com/AdamKorcz/go-fuzz-headers v0.0.0-20210312213058-32f4d319f0d2 // indirect
	github.com/cpuguy83/go-md2man/v2 v2.0.0 // indirect
	github.com/cyphar/filepath-securejoin v0.2.2 // indirect
	github.com/docker/go-units v0.4.0 // indirect
	github.com/golang/protobuf v1.5.3 // indirect
	github.com/google/go-cmp v0.5.9 // indirect
	github.com/klauspost/compress v1.16.0 // indirect
	github.com/klauspost/pgzip v1.2.4 // indirect
	github.com/opencontainers/go-digest v1.0.0 // indirect
	github.com/opencontainers/runc v1.0.0-rc90 // indirect
	github.com/opencontainers/runtime-spec v1.0.2 // indirect
	github.com/pkg/errors v0.9.1 // indirect
	github.com/rootless-containers/proto v0.1.0 // indirect
	github.com/russross/blackfriday/v2 v2.1.0 // indirect
	github.com/sirupsen/logrus v1.9.0 // indirect
	github.com/urfave/cli v1.22.4 // indirect
	github.com/vbatts/go-mtree v0.5.0 // indirect
	golang.org/x/crypto v0.7.0 // indirect
	google.golang.org/protobuf v1.33.0 // indirect
)

replace github.com/kdomanski/iso9660 => github.com/nohajc/iso9660 v0.0.0-20251105191846-0bf547744ee1

// replace github.com/kdomanski/iso9660 => ../../../3rd-party/iso9660
