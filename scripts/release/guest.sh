#!/usr/bin/env bash

set -Eeo pipefail

if [[ -f "$HOME/.zprofile" ]]; then
  # Homebrew is not on PATH for non-interactive SSH commands.
  source "$HOME/.zprofile"
fi

set -u

ALFS_RELEASE=${1:?Usage: $0 <version>}
BUILD_SCRIPT=${2:-/tmp/anylinuxfs-release/build-bottle.sh}
HOMEBREW_TAP=${3:-${ALFS_HOMEBREW_TAP:-nohajc/anylinuxfs}}
TAP_DIR=$(brew --repository "$HOMEBREW_TAP")
RELEASE_BRANCH="release/$ALFS_RELEASE"
FORMULA="$TAP_DIR/Formula/anylinuxfs.rb"

printf '%s\n' "$TAP_DIR" >"$(dirname -- "$BUILD_SCRIPT")/tap-dir"

macos_codename() {
  case "$(sw_vers -productVersion | cut -d. -f1)" in
    13) echo "ventura" ;;
    14) echo "sonoma" ;;
    15) echo "sequoia" ;;
    26) echo "tahoe" ;;
    27) echo "golden_gate" ;;
    *) return 1 ;;
  esac
}

cd "$TAP_DIR"

unexpected_changes=$(git status --porcelain --untracked-files=no | awk '$2 != "Formula/anylinuxfs.rb"')
if [[ -n "$unexpected_changes" ]]; then
  echo "Refusing to continue with unexpected tracked changes:" >&2
  printf '%s\n' "$unexpected_changes" >&2
  exit 1
fi

# An interrupted bottle command may leave an empty or uncommitted SHA. The
# bottle builder can safely recover a valid existing bottle after we sync.
git restore --staged --worktree -- Formula/anylinuxfs.rb
git fetch origin

if git show-ref --verify --quiet "refs/heads/$RELEASE_BRANCH"; then
  git switch "$RELEASE_BRANCH"
elif git show-ref --verify --quiet "refs/remotes/origin/$RELEASE_BRANCH"; then
  git switch --track -c "$RELEASE_BRANCH" "origin/$RELEASE_BRANCH"
else
  echo "Remote branch origin/$RELEASE_BRANCH does not exist" >&2
  exit 1
fi

git pull --ff-only origin "$RELEASE_BRANCH"

formula_version=$(sed -nE 's/^[[:space:]]*VERSION = "([^"]+)".*/\1/p' "$FORMULA")
if [[ "$formula_version" != "$ALFS_RELEASE" ]]; then
  echo "Expected formula version $ALFS_RELEASE, found $formula_version" >&2
  exit 1
fi

export ALFS_RELEASE TAP_DIR
bash "$BUILD_SCRIPT"

git add -- Formula/anylinuxfs.rb
if ! git diff --cached --quiet; then
  os_name=$(macos_codename)
  git commit -m "v$ALFS_RELEASE ${os_name//_/ }"
  git push origin "HEAD:$RELEASE_BRANCH"
else
  echo "The $(macos_codename) bottle is already committed"
fi

OS_NAME=$(macos_codename)
BOTTLE_FILE="$TAP_DIR/anylinuxfs-$ALFS_RELEASE.arm64_$OS_NAME.bottle.tar.gz"
[[ -s "$BOTTLE_FILE" ]] && tar -tzf "$BOTTLE_FILE" >/dev/null
