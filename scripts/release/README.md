# Homebrew release automation

This directory is the single source of truth for the macOS Homebrew bottle
release workflow.

Run a release on the newest supported macOS host:

```bash
scripts/release/release.sh <version>
```

Copy `release.conf.example` to
`${XDG_CONFIG_HOME:-$HOME/.config}/anylinuxfs/release.conf` and set the Tart VM
names and their SSH user. Machine-specific paths and VM identities belong only
in that untracked configuration.

`release.sh` builds or reuses the host bottle, starts the older macOS Tart VMs,
and copies `guest.sh` and `build-bottle.sh` into `/tmp/anylinuxfs-release` in
each VM immediately before invoking them. The VMs do not maintain independent
release-script sources.

The workflow is resumable. A stage is complete only when its bottle is a valid
gzip tar archive and its SHA-256 matches the formula. Existing branches, tags,
and GitHub releases are verified and reused. Unexpected tracked changes stop
the workflow.

The script creates or updates the GitHub release and leaves the tap's local
`main` branch ahead of `origin/main`. Review the result and run the printed
`git push origin main` command to publish the formula.

For compatibility, `~/anylinuxfs-release.sh` may be a symlink to `release.sh`;
the script resolves its real location before finding its helpers.
