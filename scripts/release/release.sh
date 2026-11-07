#!/usr/bin/env bash

set -Eeuo pipefail

ALFS_RELEASE=${1:?Usage: $0 <version>}
export ALFS_RELEASE

resolve_script_dir() {
  local source_path=${BASH_SOURCE[0]}
  local source_dir
  local link_target

  while [[ -L "$source_path" ]]; do
    source_dir=$(cd -P -- "$(dirname -- "$source_path")" && pwd)
    link_target=$(readlink "$source_path")
    if [[ "$link_target" == /* ]]; then
      source_path=$link_target
    else
      source_path="$source_dir/$link_target"
    fi
  done

  cd -P -- "$(dirname -- "$source_path")" && pwd
}

SCRIPT_DIR=$(resolve_script_dir)
CONFIG_FILE=${ALFS_RELEASE_CONFIG:-${XDG_CONFIG_HOME:-$HOME/.config}/anylinuxfs/release.conf}
if [[ ! -r "$CONFIG_FILE" ]]; then
  echo "Release configuration not found: $CONFIG_FILE" >&2
  echo "Copy $SCRIPT_DIR/release.conf.example and edit it for this host." >&2
  exit 1
fi

# shellcheck source=/dev/null
source "$CONFIG_FILE"
: "${ALFS_RELEASE_VM_USER:?Set ALFS_RELEASE_VM_USER in $CONFIG_FILE}"
if ! declare -p ALFS_RELEASE_VM_NAMES >/dev/null 2>&1 || ((${#ALFS_RELEASE_VM_NAMES[@]} == 0)); then
  echo "Set the ALFS_RELEASE_VM_NAMES array in $CONFIG_FILE" >&2
  exit 1
fi

HOMEBREW_TAP=${ALFS_HOMEBREW_TAP:-nohajc/anylinuxfs}
if [[ ! "$HOMEBREW_TAP" =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]]; then
  echo "Invalid Homebrew tap name: $HOMEBREW_TAP" >&2
  exit 1
fi

TAP_DIR=$(brew --repository "$HOMEBREW_TAP")
FORMULA="$TAP_DIR/Formula/anylinuxfs.rb"
RELEASE_BRANCH="release/$ALFS_RELEASE"
BUILD_SCRIPT="$SCRIPT_DIR/build-bottle.sh"
GUEST_SCRIPT="$SCRIPT_DIR/guest.sh"
REMOTE_RELEASE_DIR=/tmp/anylinuxfs-release
VM_NAMES=("${ALFS_RELEASE_VM_NAMES[@]}")

macos_codename_from_major() {
  case "$1" in
    13) echo "ventura" ;;
    14) echo "sonoma" ;;
    15) echo "sequoia" ;;
    26) echo "tahoe" ;;
    27) echo "golden_gate" ;;
    *)
      echo "Unsupported macOS major version: $1" >&2
      return 1
      ;;
  esac
}

macos_display_name() {
  local codename=$1
  local word
  local separator=

  for word in ${codename//_/ }; do
    printf '%s%s' "$separator" "$(printf '%s' "${word:0:1}" | tr '[:lower:]' '[:upper:]')${word:1}"
    separator=' '
  done
}

HOST_OS_NAME=$(macos_codename_from_major "$(sw_vers -productVersion | cut -d. -f1)")
BOTTLE_FILES=("anylinuxfs-$ALFS_RELEASE.arm64_$HOST_OS_NAME.bottle.tar.gz")
MACOS_NAMES=("$(macos_display_name "$HOST_OS_NAME")")

format_list() {
  local items=("$@")
  local last_index=$((${#items[@]} - 1))
  local i

  for ((i = 0; i <= last_index; i++)); do
    if ((i > 0)); then
      if ((i == last_index)); then
        printf ' and '
      else
        printf ', '
      fi
    fi
    printf '%s' "${items[$i]}"
  done
}

formula_bottle_sha() {
  local os_name=$1
  sed -nE \
    's/^[[:space:]]*sha256 cellar: :any, arm64_'"$os_name"':[[:space:]]*"([a-f0-9]{64})".*/\1/p' \
    "$FORMULA"
}

verify_bottle() {
  local bottle_file=$1
  local os_name=$2
  local expected_sha actual_sha

  [[ -s "$bottle_file" ]] && tar -tzf "$bottle_file" >/dev/null 2>&1 || {
    echo "Invalid bottle: $bottle_file" >&2
    return 1
  }

  expected_sha=$(formula_bottle_sha "$os_name")
  actual_sha=$(shasum -a 256 "$bottle_file" | awk '{print $1}')
  [[ -n "$expected_sha" && "$actual_sha" == "$expected_sha" ]] || {
    echo "Bottle SHA does not match the arm64_$os_name formula entry" >&2
    return 1
  }
}

vm_state() {
  local vm_name=$1
  tart list | awk -v vm="$vm_name" '$2 == vm {print $NF}'
}

ensure_vm_running() {
  local vm_name=$1
  local state
  state=$(vm_state "$vm_name")

  if [[ -z "$state" ]]; then
    echo "Tart VM not found: $vm_name" >&2
    return 1
  fi

  if [[ "$state" != "running" ]]; then
    echo "Starting $vm_name"
    nohup tart run --no-graphics "$vm_name" >"/tmp/tart-$vm_name-release.log" 2>&1 &
  fi
}

wait_for_ssh() {
  local vm_name=$1
  local vm_ip=
  local attempt

  for ((attempt = 1; attempt <= 90; attempt++)); do
    vm_ip=$(tart ip "$vm_name" 2>/dev/null || true)
    if [[ -n "$vm_ip" ]] && ssh \
      -o BatchMode=yes \
      -o ConnectTimeout=3 \
      -o ConnectionAttempts=1 \
      "$ALFS_RELEASE_VM_USER@$vm_ip" true 2>/dev/null; then
      printf '%s' "$vm_ip"
      return 0
    fi
    sleep 2
  done

  echo "Timed out waiting for SSH on $vm_name" >&2
  return 1
}

cd "$TAP_DIR"

unexpected_changes=$(git status --porcelain --untracked-files=no)
if [[ -n "$unexpected_changes" ]]; then
  echo "Refusing to continue with tracked changes in $TAP_DIR:" >&2
  printf '%s\n' "$unexpected_changes" >&2
  exit 1
fi

git fetch origin --tags

if git show-ref --verify --quiet "refs/heads/$RELEASE_BRANCH"; then
  git switch "$RELEASE_BRANCH"
  git pull --ff-only origin "$RELEASE_BRANCH"
elif git show-ref --verify --quiet "refs/remotes/origin/$RELEASE_BRANCH"; then
  git switch --track -c "$RELEASE_BRANCH" "origin/$RELEASE_BRANCH"
else
  git switch main
  git pull --ff-only origin main
  git switch -c "$RELEASE_BRANCH"
fi

formula_version=$(sed -nE 's/^[[:space:]]*VERSION = "([^"]+)".*/\1/p' "$FORMULA")
if [[ "$formula_version" != "$ALFS_RELEASE" ]]; then
  ./new-version.sh "$ALFS_RELEASE"
fi

TAP_DIR="$TAP_DIR" bash "$BUILD_SCRIPT"
git add -- Formula/anylinuxfs.rb
if ! git diff --cached --quiet; then
  git commit -m "v$ALFS_RELEASE golden gate"
fi
git push -u origin "$RELEASE_BRANCH"

for vm_name in "${VM_NAMES[@]}"; do
  ensure_vm_running "$vm_name"
  vm_ip=$(wait_for_ssh "$vm_name")
  vm_target="$ALFS_RELEASE_VM_USER@$vm_ip"
  vm_macos_major=$(ssh "$vm_target" 'sw_vers -productVersion | cut -d. -f1')
  macos_version=$(macos_codename_from_major "$vm_macos_major")
  macos_name=$(macos_display_name "$macos_version")
  bottle_file="anylinuxfs-$ALFS_RELEASE.arm64_$macos_version.bottle.tar.gz"
  echo "Building or recovering the $macos_name bottle on $vm_name ($vm_ip)"

  ssh "$vm_target" mkdir -p "$REMOTE_RELEASE_DIR"
  scp -q "$BUILD_SCRIPT" "$vm_target:$REMOTE_RELEASE_DIR/build-bottle.sh"
  scp -q "$GUEST_SCRIPT" "$vm_target:$REMOTE_RELEASE_DIR/guest.sh"
  ssh "$vm_target" chmod 755 \
    "$REMOTE_RELEASE_DIR/build-bottle.sh" \
    "$REMOTE_RELEASE_DIR/guest.sh"
  ssh "$vm_target" bash "$REMOTE_RELEASE_DIR/guest.sh" \
    "$ALFS_RELEASE" "$REMOTE_RELEASE_DIR/build-bottle.sh" "$HOMEBREW_TAP"

  git pull --ff-only origin "$RELEASE_BRANCH"
  remote_tap_dir=$(ssh "$vm_target" cat "$REMOTE_RELEASE_DIR/tap-dir")

  part_file="$bottle_file.part"
  rm -f -- "$part_file"
  scp -q \
    "$vm_target:$remote_tap_dir/$bottle_file" \
    "$part_file"
  verify_bottle "$part_file" "$macos_version"
  mv -f -- "$part_file" "$bottle_file"

  BOTTLE_FILES+=("$bottle_file")
  MACOS_NAMES+=("$macos_name")
done

release_tip=$(git rev-parse "$RELEASE_BRANCH")
git switch main
git pull --ff-only origin main

if ! git merge-base --is-ancestor "$release_tip" HEAD; then
  git merge --ff-only "$RELEASE_BRANCH"
fi

tag="v$ALFS_RELEASE"
if git show-ref --verify --quiet "refs/tags/$tag"; then
  tag_tip=$(git rev-list -n 1 "$tag")
  [[ "$tag_tip" == "$release_tip" ]] || {
    echo "$tag already exists at $tag_tip, expected $release_tip" >&2
    exit 1
  }
else
  git tag "$tag" "$release_tip"
fi
git push origin "refs/tags/$tag"

macos_list=$(format_list "${MACOS_NAMES[@]}")
release_title="anylinuxfs-$ALFS_RELEASE"
release_notes="Homebrew bottle for macOS $macos_list"

if gh release view "$tag" >/dev/null 2>&1; then
  gh release edit "$tag" -t "$release_title" -n "$release_notes"
  gh release upload "$tag" --clobber "${BOTTLE_FILES[@]}"
else
  gh release create "$tag" \
    -t "$release_title" \
    -n "$release_notes" \
    "${BOTTLE_FILES[@]}"
fi

echo "Release assets are ready. Review them, then publish the formula with:"
echo "  git -C $TAP_DIR push origin main"
