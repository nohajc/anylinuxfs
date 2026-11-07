#!/usr/bin/env bash

set -Eeuo pipefail

HOMEBREW_TAP=${ALFS_HOMEBREW_TAP:-nohajc/anylinuxfs}
TAP_DIR=${TAP_DIR:-$(brew --repository "$HOMEBREW_TAP")}
FORMULA="$TAP_DIR/Formula/anylinuxfs.rb"
ALFS_RELEASE=${ALFS_RELEASE:?ALFS_RELEASE must be set}

macos_codename() {
  case "$(sw_vers -productVersion | cut -d. -f1)" in
    13) echo "ventura" ;;
    14) echo "sonoma" ;;
    15) echo "sequoia" ;;
    26) echo "tahoe" ;;
    27) echo "golden_gate" ;;
    *)
      echo "Unsupported macOS version: $(sw_vers -productVersion)" >&2
      return 1
      ;;
  esac
}

formula_bottle_sha() {
  sed -nE \
    's/^[[:space:]]*sha256 cellar: :any, arm64_'"$OS_NAME"':[[:space:]]*"([a-f0-9]{64})".*/\1/p' \
    "$FORMULA"
}

set_formula_bottle_sha() {
  local new_sha=$1

  if [[ ! "$new_sha" =~ ^[a-f0-9]{64}$ ]]; then
    echo "Refusing to write an invalid bottle SHA: '$new_sha'" >&2
    return 1
  fi

  if ! grep -qE \
    '^[[:space:]]*sha256 cellar: :any, arm64_'"$OS_NAME"':[[:space:]]*"[a-f0-9]*"' \
    "$FORMULA"; then
    echo "No arm64_$OS_NAME bottle entry found in $FORMULA" >&2
    return 1
  fi

  OS_NAME="$OS_NAME" NEW_BOTTLE_SHA="$new_sha" perl -pi -e '
    my $os = $ENV{"OS_NAME"};
    my $sha = $ENV{"NEW_BOTTLE_SHA"};
    if (/^\s*sha256 cellar: :any, arm64_\Q$os\E:/) {
      s/"[a-f0-9]*"/"$sha"/;
    }
  ' "$FORMULA"

  [[ $(formula_bottle_sha) == "$new_sha" ]] || {
    echo "Failed to update the arm64_$OS_NAME bottle SHA" >&2
    return 1
  }
}

bottle_is_valid() {
  local bottle=$1
  [[ -s "$bottle" ]] && tar -tzf "$bottle" >/dev/null 2>&1
}

cd "$TAP_DIR"
OS_NAME=$(macos_codename)
BOTTLE_FILE="anylinuxfs-$ALFS_RELEASE.arm64_$OS_NAME.bottle.tar.gz"
DOUBLE_DASH_BOTTLE="anylinuxfs--$ALFS_RELEASE.arm64_$OS_NAME.bottle.tar.gz"

if bottle_is_valid "$BOTTLE_FILE"; then
  bottle_sha=$(shasum -a 256 "$BOTTLE_FILE" | awk '{print $1}')
  if [[ $(formula_bottle_sha) != "$bottle_sha" ]]; then
    echo "Reusing valid $BOTTLE_FILE and repairing its formula SHA"
    set_formula_bottle_sha "$bottle_sha"
  else
    echo "$BOTTLE_FILE is already complete"
  fi
  exit 0
fi

rm -f -- "$BOTTLE_FILE" "$DOUBLE_DASH_BOTTLE"

installed_versions=" $(brew list --versions anylinuxfs 2>/dev/null | cut -d' ' -f2-) "
if [[ "$installed_versions" == *" $ALFS_RELEASE "* ]]; then
  brew reinstall --build-bottle anylinuxfs
else
  brew install --build-bottle anylinuxfs
fi

bottle_output=$(brew bottle anylinuxfs)
printf '%s\n' "$bottle_output"

if [[ -f "$DOUBLE_DASH_BOTTLE" ]]; then
  mv -f -- "$DOUBLE_DASH_BOTTLE" "$BOTTLE_FILE"
fi

if ! bottle_is_valid "$BOTTLE_FILE"; then
  echo "brew bottle did not create a valid $BOTTLE_FILE" >&2
  exit 1
fi

bottle_sha=$(shasum -a 256 "$BOTTLE_FILE" | awk '{print $1}')
set_formula_bottle_sha "$bottle_sha"
echo "Built $BOTTLE_FILE ($bottle_sha)"
