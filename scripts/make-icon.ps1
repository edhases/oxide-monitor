# Generates resources/oxide.ico: a dark rounded square with a green ring and
# pulse glyph at 16, 32, 48 and 256 px. Small sizes use classic BMP/DIB icon
# entries; 256 px uses a PNG-compressed entry.
#
# Usage: powershell -ExecutionPolicy Bypass -File scripts\make-icon.ps1
#        powershell -ExecutionPolicy Bypass -File scripts\make-icon.ps1 -OutputPath C:\tmp\test.ico

[CmdletBinding()]
param(
    [string]$OutputPath = ""
)

$ErrorActionPreference = "Stop"
Add-Type -AssemblyName System.Drawing

if ([string]::IsNullOrWhiteSpace($OutputPath)) {
    $OutputPath = Join-Path $PSScriptRoot "..\resources\oxide.ico"
}
$OutputPath = [System.IO.Path]::GetFullPath($OutputPath)
$outputDir = Split-Path -Parent $OutputPath
if (-not (Test-Path -LiteralPath $outputDir)) {
    New-Item -ItemType Directory -Path $outputDir -Force | Out-Null
}

function New-RoundedRectPath {
    param(
        [single]$X,
        [single]$Y,
        [single]$Width,
        [single]$Height,
        [single]$Radius
    )

    $diameter = [single]([Math]::Min($Radius * 2.0, [Math]::Min($Width, $Height)))
    $path = New-Object System.Drawing.Drawing2D.GraphicsPath
    $path.AddArc($X, $Y, $diameter, $diameter, 180.0, 90.0)
    $path.AddArc($X + $Width - $diameter, $Y, $diameter, $diameter, 270.0, 90.0)
    $path.AddArc($X + $Width - $diameter, $Y + $Height - $diameter, $diameter, $diameter, 0.0, 90.0)
    $path.AddArc($X, $Y + $Height - $diameter, $diameter, $diameter, 90.0, 90.0)
    $path.CloseFigure()
    return $path
}

function New-OxideBitmap {
    param([int]$Size)

    $bitmap = New-Object System.Drawing.Bitmap($Size, $Size, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
    $backgroundBrush = $null
    $borderPen = $null
    $ringPen = $null
    $pulsePen = $null
    $path = $null

    try {
        $graphics.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
        $graphics.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
        $graphics.CompositingQuality = [System.Drawing.Drawing2D.CompositingQuality]::HighQuality
        $graphics.Clear([System.Drawing.Color]::Transparent)

        # Scale every dimension from the 256 px master drawing.
        $s = $Size / 256.0
        $inset = [Math]::Max(0.5, 6.0 * $s)
        $rectSize = [single]($Size - (2.0 * $inset))
        $radius = [single][Math]::Max(1.5, 52.0 * $s)
        $path = New-RoundedRectPath -X ([single]$inset) -Y ([single]$inset) -Width $rectSize -Height $rectSize -Radius $radius

        $backgroundBrush = New-Object System.Drawing.SolidBrush([System.Drawing.Color]::FromArgb(255, 24, 26, 29))
        $graphics.FillPath($backgroundBrush, $path)

        if ($Size -ge 24) {
            $borderPen = New-Object System.Drawing.Pen([System.Drawing.Color]::FromArgb(255, 54, 60, 67), [single][Math]::Max(1.0, 2.0 * $s))
            $graphics.DrawPath($borderPen, $path)
        }

        $center = $Size / 2.0
        $ringDiameter = [single](124.0 * $s)
        $ringWidth = [single][Math]::Max(2.0, 18.0 * $s)
        $ringRect = [System.Drawing.RectangleF]::new(
            [single]($center - ($ringDiameter / 2.0)),
            [single]($center - ($ringDiameter / 2.0)),
            $ringDiameter,
            $ringDiameter)

        $ringColor = [System.Drawing.Color]::FromArgb(255, 61, 220, 132)
        $ringPen = New-Object System.Drawing.Pen($ringColor, $ringWidth)
        $ringPen.StartCap = [System.Drawing.Drawing2D.LineCap]::Round
        $ringPen.EndCap = [System.Drawing.Drawing2D.LineCap]::Round
        $ringPen.LineJoin = [System.Drawing.Drawing2D.LineJoin]::Round
        $graphics.DrawEllipse($ringPen, $ringRect)

        # At 48 px and above add a heartbeat line inside the ring. At 16/32 px
        # the ring alone stays legible.
        if ($Size -ge 48) {
            $pulsePen = New-Object System.Drawing.Pen($ringColor, [single][Math]::Max(1.0, 9.0 * $s))
            $pulsePen.StartCap = [System.Drawing.Drawing2D.LineCap]::Round
            $pulsePen.EndCap = [System.Drawing.Drawing2D.LineCap]::Round
            $pulsePen.LineJoin = [System.Drawing.Drawing2D.LineJoin]::Round

            $points = New-Object "System.Drawing.PointF[]" 7
            $points[0] = [System.Drawing.PointF]::new([single]($center - (0.135 * $Size)), [single]$center)
            $points[1] = [System.Drawing.PointF]::new([single]($center - (0.070 * $Size)), [single]$center)
            $points[2] = [System.Drawing.PointF]::new([single]($center - (0.035 * $Size)), [single]($center - (0.085 * $Size)))
            $points[3] = [System.Drawing.PointF]::new([single]$center, [single]($center + (0.100 * $Size)))
            $points[4] = [System.Drawing.PointF]::new([single]($center + (0.035 * $Size)), [single]($center - (0.045 * $Size)))
            $points[5] = [System.Drawing.PointF]::new([single]($center + (0.070 * $Size)), [single]$center)
            $points[6] = [System.Drawing.PointF]::new([single]($center + (0.135 * $Size)), [single]$center)
            $graphics.DrawLines($pulsePen, $points)
        }
    }
    finally {
        if ($null -ne $path) { $path.Dispose() }
        if ($null -ne $pulsePen) { $pulsePen.Dispose() }
        if ($null -ne $ringPen) { $ringPen.Dispose() }
        if ($null -ne $borderPen) { $borderPen.Dispose() }
        if ($null -ne $backgroundBrush) { $backgroundBrush.Dispose() }
        $graphics.Dispose()
    }

    return $bitmap
}

function Get-PngBytes {
    param([System.Drawing.Bitmap]$Bitmap)

    $stream = New-Object System.IO.MemoryStream
    try {
        $Bitmap.Save($stream, [System.Drawing.Imaging.ImageFormat]::Png)
        # Comma prevents PowerShell from unrolling the byte array into Object[].
        return ,$stream.ToArray()
    }
    finally {
        $stream.Dispose()
    }
}

function Get-DibBytes {
    param([System.Drawing.Bitmap]$Bitmap)

    $size = $Bitmap.Width
    $maskStride = [int](([Math]::Floor(($size + 31) / 32)) * 4)
    $xorSize = $size * $size * 4
    $maskSize = $maskStride * $size
    $stream = New-Object System.IO.MemoryStream
    $writer = New-Object System.IO.BinaryWriter($stream)

    try {
        # BITMAPINFOHEADER. Height is doubled: XOR image + AND mask.
        $writer.Write([int]40)
        $writer.Write([int]$size)
        $writer.Write([int]($size * 2))
        $writer.Write([int16]1)
        $writer.Write([int16]32)
        $writer.Write([int]0)
        $writer.Write([int]($xorSize + $maskSize))
        $writer.Write([int]0)
        $writer.Write([int]0)
        $writer.Write([int]0)
        $writer.Write([int]0)

        # Bottom-up BGRA pixels.
        for ($y = $size - 1; $y -ge 0; $y--) {
            for ($x = 0; $x -lt $size; $x++) {
                $color = $Bitmap.GetPixel($x, $y)
                $writer.Write([byte]$color.B)
                $writer.Write([byte]$color.G)
                $writer.Write([byte]$color.R)
                $writer.Write([byte]$color.A)
            }
        }

        # 1-bpp AND mask, rows padded to 4 bytes, bottom-up. A set bit means
        # fully transparent; partial alpha is carried by the XOR image.
        for ($y = $size - 1; $y -ge 0; $y--) {
            $row = New-Object byte[] $maskStride
            for ($x = 0; $x -lt $size; $x++) {
                if ($Bitmap.GetPixel($x, $y).A -eq 0) {
                    $index = [int][Math]::Floor($x / 8)
                    $row[$index] = [byte]($row[$index] -bor (0x80 -shr ($x % 8)))
                }
            }
            $writer.Write($row)
        }

        $writer.Flush()
        # Comma prevents PowerShell from unrolling the byte array into Object[].
        return ,$stream.ToArray()
    }
    finally {
        $writer.Dispose()
        $stream.Dispose()
    }
}

$sizes = @(16, 32, 48, 256)
$images = @()

foreach ($size in $sizes) {
    $bitmap = New-OxideBitmap -Size $size
    try {
        if ($size -ge 256) {
            [byte[]]$bytes = Get-PngBytes -Bitmap $bitmap
        }
        else {
            [byte[]]$bytes = Get-DibBytes -Bitmap $bitmap
        }
        $images += [pscustomobject]@{
            Size  = $size
            Bytes = $bytes
        }
    }
    finally {
        $bitmap.Dispose()
    }
}

$iconStream = New-Object System.IO.MemoryStream
$iconWriter = New-Object System.IO.BinaryWriter($iconStream)
try {
    # ICONDIR
    $iconWriter.Write([int16]0)
    $iconWriter.Write([int16]1)
    $iconWriter.Write([int16]$images.Count)

    $offset = 6 + (16 * $images.Count)
    foreach ($image in $images) {
        $dimension = if ($image.Size -ge 256) { 0 } else { $image.Size }
        $iconWriter.Write([byte]$dimension)
        $iconWriter.Write([byte]$dimension)
        $iconWriter.Write([byte]0)
        $iconWriter.Write([byte]0)
        $iconWriter.Write([int16]1)
        $iconWriter.Write([int16]32)
        $iconWriter.Write([int]$image.Bytes.Length)
        $iconWriter.Write([int]$offset)
        $offset += $image.Bytes.Length
    }

    foreach ($image in $images) {
        $iconWriter.Write([byte[]]$image.Bytes)
    }

    $iconWriter.Flush()
    [System.IO.File]::WriteAllBytes($OutputPath, $iconStream.ToArray())
}
finally {
    $iconWriter.Dispose()
    $iconStream.Dispose()
}

$header = [System.IO.File]::ReadAllBytes($OutputPath)
$headerText = (($header[0..5] | ForEach-Object { $_.ToString("X2") }) -join " ")
Write-Host "Wrote $OutputPath"
Write-Host ("File size: {0} bytes" -f $header.Length)
Write-Host "Header: $headerText"
Write-Host ("Entries: " + (($images | ForEach-Object { "$($_.Size)x$($_.Size) ($($_.Bytes.Length) bytes)" }) -join ", "))
