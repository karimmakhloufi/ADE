# Generates ADE's icon: assets/ade.ico (multi-size), ade.png (256 px)
# and ade-64.rgba (raw RGBA used for the window icon).
# Usage: powershell -ExecutionPolicy Bypass -File assets\make-icon.ps1

Add-Type -AssemblyName System.Drawing
$OutDir = $PSScriptRoot

function New-IconBitmap([int]$size) {
    $bmp = New-Object System.Drawing.Bitmap $size, $size, ([System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
    $g.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
    $g.Clear([System.Drawing.Color]::Transparent)

    # Rounded square with a diagonal indigo -> violet gradient.
    $pad = [single]($size * 0.04)
    $w = [single]($size - 2 * $pad)
    $d = [single]($size * 0.44)
    $path = New-Object System.Drawing.Drawing2D.GraphicsPath
    $path.AddArc($pad, $pad, $d, $d, 180, 90)
    $path.AddArc($pad + $w - $d, $pad, $d, $d, 270, 90)
    $path.AddArc($pad + $w - $d, $pad + $w - $d, $d, $d, 0, 90)
    $path.AddArc($pad, $pad + $w - $d, $d, $d, 90, 90)
    $path.CloseFigure()
    $rect = New-Object System.Drawing.RectangleF $pad, $pad, $w, $w
    $c1 = [System.Drawing.Color]::FromArgb(255, 79, 96, 245)
    $c2 = [System.Drawing.Color]::FromArgb(255, 150, 82, 240)
    $brush = New-Object System.Drawing.Drawing2D.LinearGradientBrush $rect, $c1, $c2, 45.0
    $g.FillPath($brush, $path)

    # Prompt glyph "›_" drawn with strokes (crisp at every size, no font needed).
    $pen = New-Object System.Drawing.Pen ([System.Drawing.Color]::White), ([single][Math]::Max(1.6, $size * 0.09))
    $pen.StartCap = [System.Drawing.Drawing2D.LineCap]::Round
    $pen.EndCap = [System.Drawing.Drawing2D.LineCap]::Round
    $pen.LineJoin = [System.Drawing.Drawing2D.LineJoin]::Round
    $s = [single]$size
    $chevron = [System.Drawing.PointF[]]@(
        (New-Object System.Drawing.PointF ($s * 0.28), ($s * 0.32)),
        (New-Object System.Drawing.PointF ($s * 0.46), ($s * 0.50)),
        (New-Object System.Drawing.PointF ($s * 0.28), ($s * 0.68))
    )
    $g.DrawLines($pen, $chevron)
    $g.DrawLine($pen, [single]($s * 0.54), [single]($s * 0.68), [single]($s * 0.73), [single]($s * 0.68))

    $pen.Dispose(); $brush.Dispose(); $path.Dispose(); $g.Dispose()
    return $bmp
}

# --- .ico with PNG-compressed entries ---------------------------------------
$sizes = @(16, 24, 32, 48, 64, 128, 256)
$pngs = @()
foreach ($s in $sizes) {
    $b = New-IconBitmap $s
    $ms = New-Object System.IO.MemoryStream
    $b.Save($ms, [System.Drawing.Imaging.ImageFormat]::Png)
    if ($s -eq 256) { $b.Save((Join-Path $OutDir 'ade.png'), [System.Drawing.Imaging.ImageFormat]::Png) }
    $b.Dispose()
    $pngs += , $ms.ToArray()
}

$fs = [System.IO.File]::Create((Join-Path $OutDir 'ade.ico'))
$bw = New-Object System.IO.BinaryWriter $fs
$bw.Write([uint16]0); $bw.Write([uint16]1); $bw.Write([uint16]$sizes.Count)
$offset = 6 + 16 * $sizes.Count
for ($i = 0; $i -lt $sizes.Count; $i++) {
    $dim = if ($sizes[$i] -ge 256) { 0 } else { $sizes[$i] }
    $len = $pngs[$i].Length
    $bw.Write([byte]$dim); $bw.Write([byte]$dim); $bw.Write([byte]0); $bw.Write([byte]0)
    $bw.Write([uint16]1); $bw.Write([uint16]32)
    $bw.Write([uint32]$len); $bw.Write([uint32]$offset)
    $offset += $len
}
foreach ($p in $pngs) { $bw.Write([byte[]]$p) }
$bw.Close()

# --- raw 64x64 RGBA for iced's window icon ----------------------------------
$b = New-IconBitmap 64
$rect = New-Object System.Drawing.Rectangle 0, 0, 64, 64
$data = $b.LockBits($rect, [System.Drawing.Imaging.ImageLockMode]::ReadOnly, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
$bytes = New-Object byte[] (64 * 64 * 4)
[System.Runtime.InteropServices.Marshal]::Copy($data.Scan0, $bytes, 0, $bytes.Length)
$b.UnlockBits($data); $b.Dispose()
for ($i = 0; $i -lt $bytes.Length; $i += 4) { $t = $bytes[$i]; $bytes[$i] = $bytes[$i + 2]; $bytes[$i + 2] = $t } # BGRA -> RGBA
[System.IO.File]::WriteAllBytes((Join-Path $OutDir 'ade-64.rgba'), $bytes)

Write-Output "Icon written to $OutDir"
