# =============================================================================
#  finnCrop - the icon artwork, as drawable code
#
#  Dot-source this file, then call New-IconBitmap or New-SmallIconBitmap.  No
#  side effects on load.
#
#  Two drawings, both from the desktop sketches, both on the same tile:
#
#    Untitled-1          the large sizes.  A crop frame - two L's at opposite
#                        corners of a square, their arms running off towards
#                        the tile's edges - around a small play triangle.
#    Untitled-1 - Copy   the small sizes.  One big play triangle, nothing else,
#                        because a 64/1080 stroke is not even a pixel wide at
#                        32 px and turns the frame to grey mush.
#
#  Both numbers sets are their 1080 px artboards divided by 1080, so the
#  drawings match the sketches exactly.  All coordinates are fractions of the
#  tile side, so the same geometry draws every size.
#
#    tile          rounded square, radius 216/1080, flat #4C81EB
#
#    frame         stroke 64/1080, white, round caps and joins
#                  square 369.297..710.702 of 1080, centred
#                  arms   left/top 161.823, right 923.802, bottom 918.177
#                  play   a three-sided shape, 12.44/1080 corner radius
#
#    big play      a triangle spanning 371.3..787.5 across and 299.7..780.3
#                  down, its three points rounded by 45.57/1080
# =============================================================================

Add-Type -AssemblyName System.Drawing

$script:IconTileRadius      = 0.20000
$script:IconTileColor       = [System.Drawing.Color]::FromArgb(255, 0x4C, 0x81, 0xEB)

$script:IconStroke          = 0.05926
$script:IconFrameLo         = 0.34194
$script:IconFrameHi         = 0.65806
$script:IconArmLeft         = 0.14984
$script:IconArmRight        = 0.85537
$script:IconArmTop          = 0.14984
$script:IconArmBottom       = 0.85016

# The play triangle: three straight edges joined by a rounded corner arc at
# each point.  Anchors run clockwise from the right point; `In` is the handle
# the incoming arc arrives through, `Out` the one the next arc leaves through,
# and a null where a straight edge takes over.
$script:IconPlayX           = 0.49781
$script:IconPlayY           = 0.50000
$script:IconPlay            = @(
    @{ P = @(0.54594, 0.50998); In = @(0.55363, 0.50554); Out = $null },
    @{ P = @(0.48239, 0.54667); In = $null;               Out = @(0.47470, 0.55111) },
    @{ P = @(0.46510, 0.53670); In = @(0.46510, 0.54556); Out = $null },
    @{ P = @(0.46510, 0.46330); In = $null;               Out = @(0.46510, 0.45444) },
    @{ P = @(0.48239, 0.45333); In = @(0.47470, 0.44889); Out = $null },
    @{ P = @(0.54594, 0.49002); In = $null;               Out = @(0.55363, 0.49448) }
)

# The small sizes' mark, from Untitled-1 - Copy: one big triangle.  Same shape
# of data as $IconPlay, and scaled about the middle of its own bounding box.
$script:IconSmallPlayX      = 0.53649
$script:IconSmallPlayY      = 0.50000
$script:IconSmallPlay       = @(
    @{ P = @(0.72918, 0.53654); In = @(0.75731, 0.52030); Out = $null },
    @{ P = @(0.40709, 0.72250); In = $null;               Out = @(0.37896, 0.73874) },
    @{ P = @(0.34380, 0.68596); In = @(0.34380, 0.71844); Out = $null },
    @{ P = @(0.34380, 0.31404); In = $null;               Out = @(0.34380, 0.28156) },
    @{ P = @(0.40709, 0.27750); In = @(0.37896, 0.26126); Out = $null },
    @{ P = @(0.72918, 0.46346); In = $null;               Out = @(0.75731, 0.47970) }
)

function New-RoundedRectPath {
    param([double] $X, [double] $Y, [double] $W, [double] $H, [double] $Radius)

    $d = [Math]::Min($Radius * 2.0, [Math]::Min($W, $H))
    $path = New-Object System.Drawing.Drawing2D.GraphicsPath
    if ($d -le 0.01) {
        $path.AddRectangle((New-Object System.Drawing.RectangleF(([single] $X), ([single] $Y), ([single] $W), ([single] $H))))
        return $path
    }
    $path.AddArc([single] $X, [single] $Y, [single] $d, [single] $d, 180, 90)
    $path.AddArc([single] ($X + $W - $d), [single] $Y, [single] $d, [single] $d, 270, 90)
    $path.AddArc([single] ($X + $W - $d), [single] ($Y + $H - $d), [single] $d, [single] $d, 0, 90)
    $path.AddArc([single] $X, [single] ($Y + $H - $d), [single] $d, [single] $d, 90, 90)
    $path.CloseFigure()
    return $path
}

function New-PointF {
    param([double] $X, [double] $Y)
    return (New-Object System.Drawing.PointF(([single] $X), ([single] $Y)))
}

function Get-TrianglePoint {
    <# One point of a triangle, scaled about that triangle's own centre. #>
    param([double] $X, [double] $Y, [int] $Size, [double] $Scale, [double] $Cx, [double] $Cy)

    $px = $Cx + ($X - $Cx) * $Scale
    $py = $Cy + ($Y - $Cy) * $Scale
    return (New-PointF ($px * $Size) ($py * $Size))
}

function New-TrianglePath {
    <#
      A three-sided shape: three straight edges, each pair joined by a rounded
      corner arc.  $Anchors is the point table, $Cx/$Cy the centre to scale about.
    #>
    param([int] $Size, [double] $Scale, $Anchors, [double] $Cx, [double] $Cy)

    $a = $Anchors
    $path = New-Object System.Drawing.Drawing2D.GraphicsPath
    $pts = @()
    foreach ($anchor in $a) {
        $pts += (Get-TrianglePoint $anchor.P[0] $anchor.P[1] $Size $Scale $Cx $Cy)
    }

    # straight, rounded corner, straight, rounded corner, straight, rounded corner
    $path.AddLine($pts[0], $pts[1])
    $path.AddBezier($pts[1],
        (Get-TrianglePoint $a[1].Out[0] $a[1].Out[1] $Size $Scale $Cx $Cy),
        (Get-TrianglePoint $a[2].In[0] $a[2].In[1] $Size $Scale $Cx $Cy),
        $pts[2])
    $path.AddLine($pts[2], $pts[3])
    $path.AddBezier($pts[3],
        (Get-TrianglePoint $a[3].Out[0] $a[3].Out[1] $Size $Scale $Cx $Cy),
        (Get-TrianglePoint $a[4].In[0] $a[4].In[1] $Size $Scale $Cx $Cy),
        $pts[4])
    $path.AddLine($pts[4], $pts[5])
    $path.AddBezier($pts[5],
        (Get-TrianglePoint $a[5].Out[0] $a[5].Out[1] $Size $Scale $Cx $Cy),
        (Get-TrianglePoint $a[0].In[0] $a[0].In[1] $Size $Scale $Cx $Cy),
        $pts[0])
    $path.CloseFigure()
    return $path
}

function New-IconCanvas {
    <# A transparent bitmap with the tile already painted, ready to draw on. #>
    param([int] $Size)

    $bitmap = New-Object System.Drawing.Bitmap($Size, $Size, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
    $graphics.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
    $graphics.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
    $graphics.CompositingQuality = [System.Drawing.Drawing2D.CompositingQuality]::HighQuality
    $graphics.Clear([System.Drawing.Color]::Transparent)

    $tile = New-RoundedRectPath -X 0 -Y 0 -W $Size -H $Size -Radius ($script:IconTileRadius * $Size)
    $brush = New-Object System.Drawing.SolidBrush($script:IconTileColor)
    $graphics.FillPath($brush, $tile)
    $brush.Dispose()
    $tile.Dispose()

    return @($bitmap, $graphics)
}

function New-IconBitmap {
    <#
      The mark as drawn: crop frame around a play triangle.

      $Boost      multiplies the stroke width (1.0 is the sketch).
      $PlayScale  grows the play triangle about its centre.
      $ArmPull    pulls the four arm ends away from the tile edge, by a fraction
                  of the side, so a heavy stroke keeps its distance.
    #>
    param([int] $Size, [double] $Boost = 1.0, [double] $PlayScale = 1.0, [double] $ArmPull = 0.0)

    $canvas = New-IconCanvas -Size $Size
    $bitmap = $canvas[0]
    $graphics = $canvas[1]
    try {
        $penWidth = [Math]::Max(0.9, $script:IconStroke * $Boost * $Size)
        $pen = New-Object System.Drawing.Pen([System.Drawing.Color]::White, ([single] $penWidth))
        $pen.LineJoin = [System.Drawing.Drawing2D.LineJoin]::Round
        $pen.StartCap = [System.Drawing.Drawing2D.LineCap]::Round
        $pen.EndCap   = [System.Drawing.Drawing2D.LineCap]::Round

        $lo = $script:IconFrameLo * $Size
        $hi = $script:IconFrameHi * $Size
        $armLeft   = ($script:IconArmLeft + $ArmPull) * $Size
        $armRight  = ($script:IconArmRight - $ArmPull) * $Size
        $armTop    = ($script:IconArmTop + $ArmPull) * $Size
        $armBottom = ($script:IconArmBottom - $ArmPull) * $Size

        # top edge and left edge, corner at the square's top-left
        $graphics.DrawLines($pen, @(
            (New-PointF $armRight $lo),
            (New-PointF $lo $lo),
            (New-PointF $lo $armBottom)))
        # bottom edge and right edge, corner at the square's bottom-right
        $graphics.DrawLines($pen, @(
            (New-PointF $armLeft $hi),
            (New-PointF $hi $hi),
            (New-PointF $hi $armTop)))

        $white = New-Object System.Drawing.SolidBrush([System.Drawing.Color]::White)
        $play = New-TrianglePath -Size $Size -Scale $PlayScale `
            -Anchors $script:IconPlay -Cx $script:IconPlayX -Cy $script:IconPlayY
        $graphics.FillPath($white, $play)

        $play.Dispose()
        $white.Dispose()
        $pen.Dispose()
    } finally {
        $graphics.Dispose()
    }
    return $bitmap
}

function New-SmallIconBitmap {
    <#
      The small sizes' mark, from Untitled-1 - Copy: the tile and one big white
      play triangle, nothing else.  It needs no knock-out and no per-size tuning
      - the triangle is already most of the tile, so 16 px still reads.
    #>
    param([int] $Size, [double] $Scale = 1.0)

    $canvas = New-IconCanvas -Size $Size
    $bitmap = $canvas[0]
    $graphics = $canvas[1]
    try {
        $white = New-Object System.Drawing.SolidBrush([System.Drawing.Color]::White)
        $play = New-TrianglePath -Size $Size -Scale $Scale `
            -Anchors $script:IconSmallPlay `
            -Cx $script:IconSmallPlayX -Cy $script:IconSmallPlayY
        $graphics.FillPath($white, $play)

        $play.Dispose()
        $white.Dispose()
    } finally {
        $graphics.Dispose()
    }
    return $bitmap
}

function Get-BitmapBytes {
    <# Straight BGRA, top row first, alpha un-premultiplied. #>
    param([System.Drawing.Bitmap] $Bitmap)

    $rect = New-Object System.Drawing.Rectangle(0, 0, $Bitmap.Width, $Bitmap.Height)
    $data = $Bitmap.LockBits($rect, [System.Drawing.Imaging.ImageLockMode]::ReadOnly,
        [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    try {
        $stride = $data.Stride
        $bytes  = New-Object byte[] ($stride * $Bitmap.Height)
        [System.Runtime.InteropServices.Marshal]::Copy($data.Scan0, $bytes, 0, $bytes.Length)
    } finally {
        $Bitmap.UnlockBits($data)
    }

    # GDI+ hands back premultiplied BGRA; PNG and ICO want straight BGRA.
    for ($i = 0; $i -lt $bytes.Length; $i += 4) {
        $alpha = $bytes[$i + 3]
        if ($alpha -eq 0) {
            $bytes[$i] = 0; $bytes[$i + 1] = 0; $bytes[$i + 2] = 0
        } elseif ($alpha -ne 255) {
            foreach ($channel in 0, 1, 2) {
                $value = [int] $bytes[$i + $channel] * 255
                $bytes[$i + $channel] = [byte] [Math]::Min(255, [int] (($value + $alpha / 2) / $alpha))
            }
        }
    }
    return ,$bytes
}

function ConvertTo-DibBytes {
    param([System.Drawing.Bitmap] $Bitmap, [int] $Size)

    $bgra   = Get-BitmapBytes -Bitmap $Bitmap
    $stride = $Size * 4

    $stream = New-Object System.IO.MemoryStream
    $writer = New-Object System.IO.BinaryWriter($stream)

    $writer.Write([uint32] 40)            # BITMAPINFOHEADER
    $writer.Write([int32]  $Size)
    $writer.Write([int32]  ($Size * 2))   # XOR image plus AND mask
    $writer.Write([uint16] 1)
    $writer.Write([uint16] 32)
    $writer.Write([uint32] 0)             # BI_RGB
    $writer.Write([uint32] ($stride * $Size))
    $writer.Write([int32]  0)
    $writer.Write([int32]  0)
    $writer.Write([uint32] 0)
    $writer.Write([uint32] 0)

    for ($y = $Size - 1; $y -ge 0; --$y) {
        $writer.Write($bgra, ($y * $stride), $stride)
    }

    # AND mask: all zero, the alpha channel already describes the shape.
    $maskStride = [int] ([Math]::Floor(($Size + 31) / 32) * 4)
    $maskRow    = New-Object byte[] $maskStride
    for ($y = 0; $y -lt $Size; ++$y) {
        $writer.Write($maskRow, 0, $maskStride)
    }

    $writer.Flush()
    $result = $stream.ToArray()
    $writer.Dispose()
    $stream.Dispose()
    return ,$result
}

function ConvertTo-PngBytes {
    param([System.Drawing.Bitmap] $Bitmap)

    $stream = New-Object System.IO.MemoryStream
    $Bitmap.Save($stream, [System.Drawing.Imaging.ImageFormat]::Png)
    $result = $stream.ToArray()
    $stream.Dispose()
    return ,$result
}

function Get-RgbaBytes {
    <# Top row first, RGBA, un-premultiplied: what egui wants for a window icon. #>
    param([System.Drawing.Bitmap] $Bitmap)

    $bgra = Get-BitmapBytes -Bitmap $Bitmap
    $rgba = New-Object byte[] ($Bitmap.Width * $Bitmap.Height * 4)
    for ($i = 0; $i -lt $rgba.Length; $i += 4) {
        $rgba[$i]     = $bgra[$i + 2]
        $rgba[$i + 1] = $bgra[$i + 1]
        $rgba[$i + 2] = $bgra[$i]
        $rgba[$i + 3] = $bgra[$i + 3]
    }
    return ,$rgba
}
