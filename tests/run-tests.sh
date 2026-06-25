#!/usr/bin/env bash
# run-tests.sh — Entry point for the anylinuxfs end-to-end test suite.
#
# Prerequisites:
#   bats-core:                 macOS: brew install bats-core
#                              Debian/Ubuntu: apt install bats
#   anylinuxfs built:          ./build-app.sh  (binary at bin/anylinuxfs)
#   Alpine rootfs initialized: anylinuxfs init
#   macOS: hypervisor entitlement (local dev machine only)
#   Linux: kvm group access, util-linux losetup
#   Both:  run with sudo (some attach/mount operations are privileged)
#
# Usage:
#   ./tests/run-tests.sh                      # run all tests
#   ./tests/run-tests.sh --jobs 2             # run parallel-safe files with 2 jobs
#   ./tests/run-tests.sh --filter ext4        # run tests whose names contain "ext4"
#   ./tests/run-tests.sh --tap                # TAP output (for CI parsers)
#   ANYLINUXFS_BIN=/path/to/anylinuxfs ./tests/run-tests.sh
#
# Environment variables:
#   ANYLINUXFS_BIN                 Override path to the anylinuxfs binary (default: bin/anylinuxfs)
#   ANYLINUXFS_TEST_JOBS           Number of Bats jobs for parallel-safe files
#   ANYLINUXFS_TEST_AUTO_JOBS      Set to 1 to cap jobs by available RAM
#   ANYLINUXFS_TEST_RAM_PER_JOB_MIB  Per-job RAM budget for auto jobs (default: 1024)
#   ANYLINUXFS_RANDOM_VMNET_CIDR   Randomize vmnet CIDRs for parallel runs (default: 1 when jobs > 1)
#   ANYLINUXFS_TEST_WARM_LINUX     Warm Linux rootfs before parallel tests (default: auto)
#   ANYLINUXFS_TEST_WARM_FREEBSD   Warm FreeBSD image before parallel tests (default: auto)
#   FREEBSD_IMAGE                  FreeBSD image name used by tests (default: freebsd-15.1)
#   SKIP_APK_SETUP                 Set to 1 to skip the one-time Alpine package installation
#                                  (useful when packages are already installed from a previous run)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

ANYLINUXFS="${ANYLINUXFS_BIN:-"${REPO_ROOT}/bin/anylinuxfs"}"
FREEBSD_IMAGE="${FREEBSD_IMAGE:-freebsd-15.1}"

if [[ ! -x "$ANYLINUXFS" ]]; then
  echo "ERROR: anylinuxfs binary not found at: $ANYLINUXFS"
  echo "       Build it first with: ./build-app.sh"
  echo "       Or set ANYLINUXFS_BIN to override the path."
  exit 1
fi

if ! command -v bats &>/dev/null; then
  echo "ERROR: bats not found."
  echo "       macOS:          brew install bats-core"
  echo "       Debian/Ubuntu:  apt install bats"
  exit 1
fi

requested_jobs="${ANYLINUXFS_TEST_JOBS:-1}"
BATS_ARGS=()

while [[ $# -gt 0 ]]; do
  case "$1" in
    --jobs)
      if [[ $# -lt 2 ]]; then
        echo "ERROR: --jobs requires a value"
        exit 1
      fi
      requested_jobs="$2"
      shift 2
      ;;
    --jobs=*)
      requested_jobs="${1#--jobs=}"
      shift
      ;;
    -j)
      if [[ $# -lt 2 ]]; then
        echo "ERROR: -j requires a value"
        exit 1
      fi
      requested_jobs="$2"
      shift 2
      ;;
    -j[0-9]*)
      requested_jobs="${1#-j}"
      shift
      ;;
    *)
      BATS_ARGS+=("$1")
      shift
      ;;
  esac
done

if ! [[ "$requested_jobs" =~ ^[0-9]+$ ]] || [[ "$requested_jobs" -lt 1 ]]; then
  echo "ERROR: jobs must be a positive integer, got: $requested_jobs"
  exit 1
fi

available_ram_mib() {
  case "$(uname -s)" in
    Darwin)
      local mem_bytes
      mem_bytes="$(sysctl -n hw.memsize 2>/dev/null || true)"
      if [[ -n "$mem_bytes" ]]; then
        echo $(( mem_bytes / 1024 / 1024 ))
      else
        vm_stat 2>/dev/null | awk '
          /page size of/ { page_size = $8 }
          /Pages free:/ { free = $3 }
          /Pages inactive:/ { inactive = $3 }
          /Pages speculative:/ { speculative = $3 }
          END {
            gsub(/\./, "", free)
            gsub(/\./, "", inactive)
            gsub(/\./, "", speculative)
            if (page_size > 0) {
              print int((free + inactive + speculative) * page_size / 1024 / 1024)
            }
          }'
      fi
      ;;
    Linux)
      awk '/MemAvailable:/ {print int($2 / 1024)}' /proc/meminfo 2>/dev/null
      ;;
  esac
}

if [[ "${ANYLINUXFS_TEST_AUTO_JOBS:-0}" == "1" ]]; then
  ram_per_job_mib="${ANYLINUXFS_TEST_RAM_PER_JOB_MIB:-1024}"
  if ! [[ "$ram_per_job_mib" =~ ^[0-9]+$ ]] || [[ "$ram_per_job_mib" -lt 1 ]]; then
    echo "ERROR: ANYLINUXFS_TEST_RAM_PER_JOB_MIB must be a positive integer, got: $ram_per_job_mib"
    exit 1
  fi

  ram_mib="$(available_ram_mib || true)"
  if [[ -n "${ram_mib:-}" && "$ram_mib" -gt 0 ]]; then
    auto_jobs=$(( ram_mib / ram_per_job_mib ))
    [[ "$auto_jobs" -lt 1 ]] && auto_jobs=1
    if [[ "$requested_jobs" -eq 1 || "$auto_jobs" -lt "$requested_jobs" ]]; then
      requested_jobs="$auto_jobs"
    fi
    echo "==> Auto jobs: ${requested_jobs} (${ram_mib} MiB available, ${ram_per_job_mib} MiB/job)"
  else
    echo "WARNING: Could not determine available RAM; using ${requested_jobs} job(s)"
  fi
fi

if [[ "$requested_jobs" -gt 1 ]]; then
  export ANYLINUXFS_RANDOM_VMNET_CIDR="${ANYLINUXFS_RANDOM_VMNET_CIDR:-1}"

  if ! command -v parallel &>/dev/null && ! command -v rush &>/dev/null; then
    echo "ERROR: Bats parallel jobs require GNU parallel or rush."
    echo "       macOS: brew install parallel"
    echo "       Or rerun with --jobs 1."
    exit 1
  fi
fi

# ---------------------------------------------------------------------------
# One-time Alpine package installation
# These packages are needed by the VM shell to format disk images.
# ---------------------------------------------------------------------------
APK_PACKAGES=(
  parted          # partition tables (GPT, MBR)
  e2fsprogs       # mkfs.ext4, e2fsck
  btrfs-progs     # mkfs.btrfs, btrfs
  exfatprogs      # mkfs.exfat
  f2fs-tools      # mkfs.f2fs
  ntfs-3g-progs   # mount.ntfs-3g
  cryptsetup      # LUKS encryption
  lvm2            # pvcreate, vgcreate, lvcreate
)

if [[ "${SKIP_APK_SETUP:-0}" != "1" ]]; then
  echo "==> Installing Alpine packages into anylinuxfs rootfs..."
  echo "    (Set SKIP_APK_SETUP=1 to skip if already installed)"
  "$ANYLINUXFS" apk add "${APK_PACKAGES[@]}"
  echo "==> Alpine packages installed."
else
  echo "==> Skipping Alpine package installation (SKIP_APK_SETUP=1)"
fi

filter_matches() {
  local pattern="$1"
  local arg next_is_filter=0 filter_seen=0 filter_pattern=""

  for arg in "${BATS_ARGS[@]}"; do
    if [[ "$next_is_filter" -eq 1 ]]; then
      filter_pattern="$arg"
      filter_seen=1
      next_is_filter=0
      continue
    fi
    case "$arg" in
      --filter)
        next_is_filter=1
        ;;
      --filter=*)
        filter_pattern="${arg#--filter=}"
        filter_seen=1
        ;;
    esac
  done

  if [[ "$filter_seen" -eq 0 ]]; then
    return 0
  fi

  [[ "${filter_pattern,,}" =~ $pattern ]]
}

should_warm_linux() {
  case "${ANYLINUXFS_TEST_WARM_LINUX:-auto}" in
    1|true|yes)
      return 0
      ;;
    0|false|no)
      return 1
      ;;
  esac

  if [[ "$requested_jobs" -eq 1 ]]; then
    return 1
  fi

  filter_matches "(ext4|btrfs|exfat|f2fs|ntfs|lvm|luks|partition|raid|multi|attach|qcow|zfs|ufs|keyfile|freebsd)"
}

should_warm_freebsd() {
  case "${ANYLINUXFS_TEST_WARM_FREEBSD:-auto}" in
    1|true|yes)
      return 0
      ;;
    0|false|no)
      return 1
      ;;
  esac

  if [[ "$requested_jobs" -eq 1 ]]; then
    return 1
  fi

  filter_matches "(freebsd|zfs|ufs|keyfile)"
}

if should_warm_linux; then
  echo "==> Warming Linux rootfs before parallel tests..."
  "$ANYLINUXFS" shell -c true
  echo "==> Linux rootfs is warm."
fi

if should_warm_freebsd; then
  echo "==> Warming FreeBSD image ${FREEBSD_IMAGE} before parallel tests..."
  "$ANYLINUXFS" shell -i "$FREEBSD_IMAGE" -c true
  echo "==> FreeBSD image is warm."
fi

# ---------------------------------------------------------------------------
# Run bats, forwarding any extra arguments (--filter, --tap, etc.).
# The default remains sequential. When jobs > 1, files with global-state
# assumptions run serially and the remaining files run in parallel across
# files only; within-file tests stay serialized because many files share
# setup_file() fixtures.
# ---------------------------------------------------------------------------
BATS_FILES=("${SCRIPT_DIR}"/*.bats)

if [[ ${#BATS_FILES[@]} -eq 0 || ! -f "${BATS_FILES[0]}" ]]; then
  echo "ERROR: No .bats files found in ${SCRIPT_DIR}"
  exit 1
fi

# Serial-only files:
# - 20-subcommands.bats asserts `anylinuxfs status` is empty.
# - 21-mount-options.bats edits the real user config file.
SERIAL_BATS_FILES=(
  "${SCRIPT_DIR}/20-subcommands.bats"
  "${SCRIPT_DIR}/21-mount-options.bats"
)

if [[ "$requested_jobs" -eq 1 ]]; then
  echo "==> Running ${#BATS_FILES[@]} test file(s) sequentially with bats..."
  exec bats "${BATS_ARGS[@]}" "${BATS_FILES[@]}"
fi

PARALLEL_BATS_FILES=()
for file in "${BATS_FILES[@]}"; do
  serial=0
  for serial_file in "${SERIAL_BATS_FILES[@]}"; do
    if [[ "$file" == "$serial_file" ]]; then
      serial=1
      break
    fi
  done
  [[ "$serial" -eq 0 ]] && PARALLEL_BATS_FILES+=("$file")
done

echo "==> Running ${#SERIAL_BATS_FILES[@]} serial test file(s) with bats..."
bats "${BATS_ARGS[@]}" "${SERIAL_BATS_FILES[@]}"

echo "==> Running ${#PARALLEL_BATS_FILES[@]} parallel-safe test file(s) with bats -j ${requested_jobs}..."
exec bats \
  --jobs "$requested_jobs" \
  --no-parallelize-within-files \
  "${BATS_ARGS[@]}" \
  "${PARALLEL_BATS_FILES[@]}"
