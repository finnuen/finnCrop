# =============================================================================
#  finnCrop - one-click build
#
#  The version lives in Cargo.toml and nowhere else. This script bumps it,
#  builds, and copies the result to dist\ under a name that says which build it
#  is:
#
#      dist\finnCrop_v1.2.exe
#
#  finnCrop publishes two digits, so the last digit that moves is the minor one
#  (1.2 -> 1.3). Cargo itself insists on three parts, which is why Cargo.toml
#  reads "1.2.0" and the trailing part is always zero; build.rs trims it back to
#  "1.2" for the title bar, the About window and the version block.
#
#  Usage:
#    .\build.ps1                  release build, version bumped
#    .\build.ps1 -Debug           debug build, version bumped
#    .\build.ps1 -KeepVersion     rebuild the current version unchanged
#    .\build.ps1 -Version 2.0     set the version outright (nothing is bumped)
#    .\build.ps1 -Clean           remove the build tree first
#
#  FFmpeg: ffmpeg.exe and ffprobe.exe from vendor\ are packed into the
#  executable. If they are not there yet they are downloaded once (about 110 MB)
#  from the one pinned gyan.dev package named below, and the download is checked
#  against that package's published SHA256 before anything uses it.
#
#  The pin matters twice over. GPL v3 asks for the source that corresponds to
#  the binary actually shipped, so the version cannot drift silently under a
#  document that names one version; and a fixed checksum is what makes "the
#  same build" mean something.
# =============================================================================

#  The -Debug switch is deliberately not [CmdletBinding()]: with that attribute
#  PowerShell adds its own common -Debug parameter and the two collide.

param(
    [switch] $Debug,
    [switch] $KeepVersion,
    [string] $Version,
    [switch] $Clean
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$root = $PSScriptRoot
$cargoToml = Join-Path $root 'Cargo.toml'
$vendorDir = Join-Path $root 'vendor'
$distDir = Join-Path $root 'dist'

# The FFmpeg build that is packed into every release, named exactly rather than
# left as "whatever is latest": the release notes and THIRD-PARTY.md point at the
# source for this version, and they would stop being true if this URL floated.
#
# The hash is the one gyan.dev publishes at
# https://www.gyan.dev/ffmpeg/builds/packages/ffmpeg-9.0.2-essentials_build.zip.sha256
$ffmpegVersion = '9.0.2'
$ffmpegUrl     = 'https://www.gyan.dev/ffmpeg/builds/packages/ffmpeg-9.0.2-essentials_build.zip'
$ffmpegSha256  = '60f467265b1e312373dbcd92200c2618a74850f98d3d078e94296bb3fa2047ba'

# --- the version, from the one place that holds it ---------------------------

function Get-ManifestVersion {
    $lines = [System.IO.File]::ReadAllLines($cargoToml)
    $inPackage = $false
    foreach ($line in $lines) {
        if ($line -match '^\s*\[\s*([^\]]+)\s*\]') {
            $inPackage = ($Matches[1] -eq 'package')
            continue
        }
        if ($inPackage -and $line -match '^\s*version\s*=\s*"([^"]+)"') {
            return $Matches[1]
        }
    }
    throw "No version = in the [package] section of $cargoToml"
}

function Set-ManifestVersion {
    param([string] $New)

    $lines = [System.IO.File]::ReadAllLines($cargoToml)
    $inPackage = $false
    $done = $false
    for ($i = 0; $i -lt $lines.Count; ++$i) {
        if ($lines[$i] -match '^\s*\[\s*([^\]]+)\s*\]') {
            $inPackage = ($Matches[1] -eq 'package')
            continue
        }
        if ($inPackage -and $lines[$i] -match '^(\s*version\s*=\s*")[^"]+(".*)$') {
            $lines[$i] = $Matches[1] + $New + $Matches[2]
            $done = $true
            break
        }
    }
    if (-not $done) { throw "Could not write the version into $cargoToml" }
    [System.IO.File]::WriteAllLines($cargoToml, $lines)
}

# MAJOR.MINOR.PATCH in the manifest; MAJOR.MINOR is what finnCrop publishes.
function Format-Version {
    param([string[]] $Parts)
    return ('{0}.{1}' -f $Parts[0], $Parts[1])
}

$manifestVersion = Get-ManifestVersion
$parts = $manifestVersion.Split('.')

if ($Version) {
    if ($Version -notmatch '^[0-9]+\.[0-9]+(\.[0-9]+)?$') {
        throw "-Version needs a number like 1.2, or 1.2.3. Got '$Version'."
    }
    $newParts = $Version.Split('.')
    while ($newParts.Count -lt 3) { $newParts += '0' }
    $newManifest = $newParts -join '.'
} elseif ($KeepVersion) {
    $newManifest = $manifestVersion
} else {
    if ($parts.Count -lt 2) { throw "Unreadable version '$manifestVersion' in Cargo.toml" }
    $minor = [int] $parts[1] + 1
    $newManifest = '{0}.{1}.0' -f $parts[0], $minor
}

if ($newManifest -ne $manifestVersion) {
    Set-ManifestVersion -New $newManifest
    Write-Host "Version $manifestVersion -> $newManifest"
}
$shortVersion = Format-Version -Parts $newManifest.Split('.')

# --- FFmpeg, once -------------------------------------------------------------

function Get-VendorBinaries {
    $wanted = @('ffmpeg.exe', 'ffprobe.exe')
    $missing = $wanted | Where-Object { -not (Test-Path (Join-Path $vendorDir $_)) }
    if (-not $missing) { return }

    Write-Host ''
    Write-Host ("Downloading FFmpeg {0} once ({1})..." -f $ffmpegVersion, $ffmpegUrl)
    New-Item -ItemType Directory -Force -Path $vendorDir | Out-Null
    $zip = Join-Path $env:TEMP 'finnCrop-ffmpeg-essentials.zip'
    Invoke-WebRequest -Uri $ffmpegUrl -OutFile $zip -UseBasicParsing

    # The download is checked before it is unpacked: a substituted or truncated
    # archive must not end up inside a release.
    $actual = (Get-FileHash $zip -Algorithm SHA256).Hash.ToLower()
    if ($actual -ne $ffmpegSha256) {
        Add-Type -AssemblyName Microsoft.VisualBasic
        [Microsoft.VisualBasic.FileIO.FileSystem]::DeleteFile($zip, 'OnlyErrorDialogs', 'SendToRecycleBin')
        throw "FFmpeg download failed its checksum.`n  expected $ffmpegSha256`n  got      $actual"
    }
    Write-Host ("  sha256 ok  {0}" -f $actual)

    $staging = Join-Path $env:TEMP 'finnCrop-ffmpeg-unpack'
    if (Test-Path $staging) {
        Add-Type -AssemblyName Microsoft.VisualBasic
        [Microsoft.VisualBasic.FileIO.FileSystem]::DeleteDirectory($staging, 'OnlyErrorDialogs', 'SendToRecycleBin')
    }
    Expand-Archive -Path $zip -DestinationPath $staging -Force
    foreach ($name in $wanted) {
        $found = Get-ChildItem $staging -Recurse -Filter $name -File | Select-Object -First 1
        if (-not $found) { throw "$name is not in the archive from $ffmpegUrl" }
        Copy-Item $found.FullName (Join-Path $vendorDir $name) -Force
        Write-Host ("  vendor\{0}  {1:N1} MB" -f $name, ((Get-Item $found.FullName).Length / 1MB))
    }

    Add-Type -AssemblyName Microsoft.VisualBasic
    [Microsoft.VisualBasic.FileIO.FileSystem]::DeleteDirectory($staging, 'OnlyErrorDialogs', 'SendToRecycleBin')
    [Microsoft.VisualBasic.FileIO.FileSystem]::DeleteFile($zip, 'OnlyErrorDialogs', 'SendToRecycleBin')
}

Get-VendorBinaries

# --- build --------------------------------------------------------------------

$cargo = Join-Path $env:USERPROFILE '.cargo\bin\cargo.exe'
if (-not (Test-Path $cargo)) { $cargo = 'cargo' }

if ($Clean) {
    $target = Join-Path $root 'target'
    if (Test-Path $target) {
        Write-Host 'Removing the previous build tree...'
        Add-Type -AssemblyName Microsoft.VisualBasic
        [Microsoft.VisualBasic.FileIO.FileSystem]::DeleteDirectory($target, 'OnlyErrorDialogs', 'SendToRecycleBin')
    }
}

$profile = if ($Debug) { 'debug' } else { 'release' }
$exeName = "finnCrop_v$shortVersion.exe"
$built = Join-Path $root "target\$profile\finnCrop.exe"

Write-Host ''
Write-Host ("Building ({0})..." -f $profile)
$arguments = @('build')
if (-not $Debug) { $arguments += '--release' }
& $cargo @arguments
if ($LASTEXITCODE -ne 0) { throw "cargo build failed ($LASTEXITCODE)" }

if (-not (Test-Path $built)) { throw "$built was not produced" }

New-Item -ItemType Directory -Force -Path $distDir | Out-Null
Copy-Item $built (Join-Path $distDir $exeName) -Force

$out = Join-Path $distDir $exeName
Write-Host ''
Write-Host '==========================================================================='
Write-Host (" Build finished: version {0}, {1}" -f $shortVersion, $profile)
Write-Host '==========================================================================='
Write-Host (" {0}  ({1:N1} MB)" -f $out, ((Get-Item $out).Length / 1MB))
Write-Host ' FFmpeg and ffprobe are inside; on first run they are unpacked once to'
Write-Host ' %LOCALAPPDATA%\finnCrop\ffmpeg\<version>\.'
Write-Host '==========================================================================='
