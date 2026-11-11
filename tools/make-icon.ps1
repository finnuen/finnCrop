# =============================================================================
#  finnCrop - builds app-icon.png, app.ico and the runtime window icon
#
#  The artwork itself lives in IconDraw.ps1 (same folder), so this file only
#  decides the frames and writes the files:
#
#    src/app-icon.png   master artwork, 1024 px — the source of truth
#    src/app.ico        multi-frame icon, embedded by build.rs
#    src/icon-64.rgba   raw RGBA for the window icon at runtime (the app then
#                       needs no image decoder).  Not the 64 px frame: see below.
#
#  Frame storage: 16-48 px as uncompressed DIBs, because parts of the shell
#  still refuse the PNG variant at small sizes; 64 px and up as PNG.
#
#  Two drawings are involved.  16-32 px get the small mark from Untitled-1 -
#  Copy - the tile and one big play triangle - because the crop frame's
#  64/1080 stroke is under a pixel wide there and turns to grey mush.  48 px
#  and up get the frame itself, drawn at size up to 48 and scaled from the
#  master above that.
#
#  The runtime icon is drawn from the small mark too.  Windows is handed one
#  bitmap for the window and shrinks it itself for the title bar and the
#  taskbar, so the frame would put the mush straight back.
#
#  Usage: powershell -ExecutionPolicy Bypass -File tools\make-icon.ps1
# =============================================================================

[CmdletBinding()]
param()

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$scriptDirectory = Split-Path -Parent $MyInvocation.MyCommand.Path
$projectRoot     = Split-Path -Parent $scriptDirectory
$pngPath         = Join-Path $projectRoot 'src\app-icon.png'
$icoPath         = Join-Path $projectRoot 'src\app.ico'
$rgbaPath        = Join-Path $projectRoot 'src\icon-64.rgba'

. (Join-Path $scriptDirectory 'IconDraw.ps1')

$sizes       = @(16, 24, 32, 48, 64, 128, 256)
$masterSize  = 1024

# Sizes at or below this are the small mark (Untitled-1 - Copy): the tile and
# one big play triangle.  Above it, the crop frame.
$smallAtOrBelow = 32

# --- master artwork -----------------------------------------------------------
$master = New-IconBitmap -Size $masterSize
try {
    $master.Save($pngPath, [System.Drawing.Imaging.ImageFormat]::Png)
} finally {
    $master.Dispose()
}

# --- one bitmap per frame -----------------------------------------------------
$frames = @()
foreach ($size in $sizes) {
    if ($size -le $smallAtOrBelow) {
        $bitmap = New-SmallIconBitmap -Size $size
    } elseif ($size -le 48) {
        # Drawn at its own size: cleaner than a 48-way downscale of the master.
        $bitmap = New-IconBitmap -Size $size
    } else {
        # Scaled from the master so the gradient and the curves stay smooth.
        $source = [System.Drawing.Image]::FromFile($pngPath)
        try {
            $bitmap = New-Object System.Drawing.Bitmap($size, $size, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
            $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
            try {
                $graphics.SmoothingMode      = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
                $graphics.InterpolationMode  = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
                $graphics.PixelOffsetMode    = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
                $graphics.CompositingQuality = [System.Drawing.Drawing2D.CompositingQuality]::HighQuality
                $graphics.Clear([System.Drawing.Color]::Transparent)
                $graphics.DrawImage($source, (New-Object System.Drawing.Rectangle(0, 0, $size, $size)))
            } finally {
                $graphics.Dispose()
            }
        } finally {
            $source.Dispose()
        }
    }

    try {
        $bytes = if ($size -le 48) {
            ConvertTo-DibBytes -Bitmap $bitmap -Size $size
        } else {
            ConvertTo-PngBytes -Bitmap $bitmap
        }
        $frames += [pscustomobject]@{ Size = $size; Bytes = $bytes }
    } finally {
        $bitmap.Dispose()
    }
}

# --- the runtime window icon --------------------------------------------------
# Windows shrinks this one bitmap itself for the title bar and the taskbar, so
# it is the small mark: the same drawing the 16-32 px frames use, with enough
# detail left after the shrink to still read.
$windowBitmap = New-SmallIconBitmap -Size 64
try {
    [System.IO.File]::WriteAllBytes($rgbaPath, (Get-RgbaBytes -Bitmap $windowBitmap))
} finally {
    $windowBitmap.Dispose()
}

# --- assemble the .ico container ---------------------------------------------
$output = New-Object System.IO.MemoryStream
$writer = New-Object System.IO.BinaryWriter($output)

$writer.Write([uint16] 0)                 # reserved
$writer.Write([uint16] 1)                 # type: icon
$writer.Write([uint16] $frames.Count)

$offset = 6 + 16 * $frames.Count
foreach ($frame in $frames) {
    $dimension = if ($frame.Size -ge 256) { 0 } else { $frame.Size }
    $writer.Write([byte]   $dimension)
    $writer.Write([byte]   $dimension)
    $writer.Write([byte]   0)             # palette size
    $writer.Write([byte]   0)             # reserved
    $writer.Write([uint16] 1)             # colour planes
    $writer.Write([uint16] 32)            # bits per pixel
    $writer.Write([uint32] $frame.Bytes.Length)
    $writer.Write([uint32] $offset)
    $offset += $frame.Bytes.Length
}
foreach ($frame in $frames) {
    $writer.Write($frame.Bytes, 0, $frame.Bytes.Length)
}

$writer.Flush()
$bytes = $output.ToArray()
$writer.Dispose()
$output.Dispose()
[System.IO.File]::WriteAllBytes($icoPath, $bytes)

Write-Host ("Wrote {0} ({1} bytes, {2} frames: {3})" -f $icoPath, $bytes.Length, $frames.Count, ($sizes -join ', '))
Write-Host ("Wrote {0} ({1}x{1})" -f $pngPath, $masterSize)
Write-Host ("Wrote {0} (64x64 RGBA, {1} bytes)" -f $rgbaPath, (64 * 64 * 4))
