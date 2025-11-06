module anylinuxfs/freebsd-bootstrap

go 1.25.2

require github.com/kdomanski/iso9660 v0.4.0

require (
	github.com/moby/sys/mount v0.3.4 // indirect
	github.com/moby/sys/mountinfo v0.7.2 // indirect
	golang.org/x/sys v0.1.0 // indirect
)

replace github.com/kdomanski/iso9660 => github.com/nohajc/iso9660 v0.0.0-20251105191846-0bf547744ee1

// replace github.com/kdomanski/iso9660 => ../../../3rd-party/iso9660
