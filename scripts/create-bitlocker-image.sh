#!/usr/bin/env bash
set -euo pipefail

## Accept remote host and image name from command-line arguments.
## IMAGE_NAME should be provided without the file extension.
remote_host="${1:-winsrv}"
image_name="${2:-test-bitlocker}"
image_name_vhdx="${image_name}.vhdx"
image_path="${IMAGE_PATH:-C:\\Mac\\Home\\Documents\\BitLockerImages\\${image_name_vhdx}}"
host_image_path="${HOME}/Documents/BitLockerImages/${image_name_vhdx}"
host_image_raw_path="${host_image_path%.*}.img"
size_gb="${SIZE_GB:-1}"
drive_letter="${DRIVE_LETTER:-R}"
volume_label="${VOLUME_LABEL:-TestBitLocker}"

ps_single_quote() {
    local value="${1//\'/\'\'}"
    printf "'%s'" "$value"
}

remote_image_path="$(ps_single_quote "$image_path")"
remote_drive_letter="$(ps_single_quote "$drive_letter")"
remote_volume_label="$(ps_single_quote "$volume_label")"
size_mb="$((size_gb * 1024))"

ssh "$remote_host" pwsh -NoLogo -NonInteractive -Command - <<EOF
\$ErrorActionPreference = 'Stop'

\$imagePath = $remote_image_path
\$sizeMb = $size_mb
\$driveLetter = $remote_drive_letter
\$volumeLabel = $remote_volume_label

function Write-Log {
    param([string]\$Message)
    [Console]::Error.WriteLine(\$Message)
}

if (Test-Path -LiteralPath \$imagePath) {
    \$diskImage = Get-DiskImage -ImagePath \$imagePath -ErrorAction SilentlyContinue
    if (\$diskImage -and \$diskImage.Attached) {
        Dismount-DiskImage -ImagePath \$imagePath | Out-Null
        Start-Sleep -Seconds 1
    }

    Remove-Item -LiteralPath \$imagePath -Force
}

\$parent = Split-Path -Parent \$imagePath
if (\$parent -and -not (Test-Path -LiteralPath \$parent)) {
    New-Item -ItemType Directory -Path \$parent -Force | Out-Null
}

\$extension = [System.IO.Path]::GetExtension(\$imagePath)
if (-not \$extension) {
    \$extension = '.vhdx'
}

\$workingImagePath = Join-Path ([System.IO.Path]::GetTempPath()) ("anylinuxfs-" + [System.Guid]::NewGuid().ToString('N') + \$extension)

\$diskAttached = \$false
\$recoveryPassword = \$null
try {

\$diskpartScript = @"
create vdisk file="\$workingImagePath" maximum=\$sizeMb type=expandable
select vdisk file="\$workingImagePath"
attach vdisk
"@
\$diskpartScript | & diskpart 2>&1 | ForEach-Object { Write-Log \$_ }
\$diskAttached = \$true

Start-Sleep -Seconds 2

\$disk = Get-DiskImage -ImagePath \$workingImagePath | Get-Disk | Select-Object -First 1
if (-not \$disk) {
    throw "Unable to locate the attached VHD for \$workingImagePath"
}

Initialize-Disk -Number \$disk.Number -PartitionStyle GPT | Out-Null
New-Partition -DiskNumber \$disk.Number -UseMaximumSize -DriveLetter \$driveLetter | Out-Null
Format-Volume -DriveLetter \$driveLetter -FileSystem NTFS -NewFileSystemLabel \$volumeLabel -Confirm:\$false | Out-Null

Write-Log "Starting BitLocker on \$workingImagePath"
Enable-BitLocker -MountPoint "\${driveLetter}:" -RecoveryPasswordProtector -UsedSpaceOnly -SkipHardwareTest -WarningAction SilentlyContinue | Out-Null

Start-Sleep -Seconds 1

\$recoveryPassword = Get-BitLockerVolume -MountPoint "\${driveLetter}:" |
    Select-Object -ExpandProperty KeyProtector |
    Where-Object KeyProtectorType -eq 'RecoveryPassword' |
    Select-Object -First 1 -ExpandProperty RecoveryPassword

if (-not \$recoveryPassword) {
    throw "Unable to determine the recovery password for \${driveLetter}:"
}

} finally {
    if (\$diskAttached) {
        Dismount-DiskImage -ImagePath \$workingImagePath -ErrorAction SilentlyContinue | Out-Null
    }
}

if (\$workingImagePath -ne \$imagePath) {
    Move-Item -LiteralPath \$workingImagePath -Destination \$imagePath -Force
}

Write-Output \$recoveryPassword

EOF

# The empty line between last closing brace and EOF is crucial.
# Without it, the powershell script didn't work at all.

# Ensure any previous raw image is removed before conversion
if [ -f "$host_image_raw_path" ]; then
    rm -f "$host_image_raw_path" || true
fi

qemu-img convert -f vhdx -O raw "$host_image_path" "$host_image_raw_path"
