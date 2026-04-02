#!/usr/bin/env bash
set -euo pipefail

remote_host="${REMOTE_HOST:-winsrv}"
image_path="${IMAGE_PATH:-C:\\BitLockerImages\\test-bitlocker.vhdx}"
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
    New-Item -ItemType Directory -Path \$parent | Out-Null
}

\$diskAttached = \$false
try {

\$diskpartScript = @"
create vdisk file="\$imagePath" maximum=\$sizeMb type=fixed
select vdisk file="\$imagePath"
attach vdisk
"@
\$diskpartScript | & diskpart 2>&1 | ForEach-Object { Write-Log \$_ }
\$diskAttached = \$true

Start-Sleep -Seconds 2

\$disk = Get-DiskImage -ImagePath \$imagePath | Get-Disk | Select-Object -First 1
if (-not \$disk) {
    throw "Unable to locate the attached VHD for \$imagePath"
}

Initialize-Disk -Number \$disk.Number -PartitionStyle GPT | Out-Null
New-Partition -DiskNumber \$disk.Number -UseMaximumSize -DriveLetter \$driveLetter | Out-Null
Format-Volume -DriveLetter \$driveLetter -FileSystem NTFS -NewFileSystemLabel \$volumeLabel -Confirm:\$false | Out-Null

Write-Log "Starting BitLocker on \$imagePath"
Enable-BitLocker -MountPoint "\${driveLetter}:" -RecoveryPasswordProtector -UsedSpaceOnly -SkipHardwareTest -WarningAction SilentlyContinue | Out-Null

Start-Sleep -Seconds 1

\$recoveryPassword = Get-BitLockerVolume -MountPoint "\${driveLetter}:" |
    Select-Object -ExpandProperty KeyProtector |
    Where-Object KeyProtectorType -eq 'RecoveryPassword' |
    Select-Object -First 1 -ExpandProperty RecoveryPassword

if (-not \$recoveryPassword) {
    throw "Unable to determine the recovery password for \${driveLetter}:"
}

Write-Output \$recoveryPassword

} finally {
    if (\$diskAttached) {
        Dismount-DiskImage -ImagePath \$imagePath -ErrorAction SilentlyContinue | Out-Null
    }
}

EOF

# The empty line between last closing brace and EOF is crucial.
# Without it, the powershell script didn't work at all.
