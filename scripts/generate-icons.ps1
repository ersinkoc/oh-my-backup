$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing
$assetDir = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../assets'))
[System.IO.Directory]::CreateDirectory($assetDir) | Out-Null
function RoundedPath([single]$x, [single]$y, [single]$w, [single]$h, [single]$r) {
    $p = [System.Drawing.Drawing2D.GraphicsPath]::new()
    $d = $r * 2
    $p.AddArc($x, $y, $d, $d, 180, 90)
    $p.AddArc(($x+$w-$d), $y, $d, $d, 270, 90)
    $p.AddArc(($x+$w-$d), ($y+$h-$d), $d, $d, 0, 90)
    $p.AddArc($x, ($y+$h-$d), $d, $d, 90, 90)
    $p.CloseFigure()
    return $p
}
$images = @()
foreach ($size in @(16, 20, 24, 32, 40, 48, 64, 128, 256)) {
    $bmp = [System.Drawing.Bitmap]::new($size, $size)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.SmoothingMode = 'AntiAlias'
    $g.ScaleTransform(($size/256.0), ($size/256.0))
    $navy = [System.Drawing.SolidBrush]::new([System.Drawing.ColorTranslator]::FromHtml('#102537'))
    $mint = [System.Drawing.SolidBrush]::new([System.Drawing.ColorTranslator]::FromHtml('#48dfc0'))
    $white = [System.Drawing.SolidBrush]::new([System.Drawing.ColorTranslator]::FromHtml('#eaf6fc'))
    $pen = [System.Drawing.Pen]::new($mint, 16)
    $pen.StartCap = 'Round'; $pen.EndCap = 'Round'
    $p = RoundedPath 8 8 240 240 56
    $g.FillPath($navy, $p); $p.Dispose()
    $p = [System.Drawing.Drawing2D.GraphicsPath]::new()
    $p.AddLine(64,100,64,76); $p.AddBezier(64,76,64,52,64,52,88,52); $p.AddLine(88,52,156,52)
    $g.DrawPath($pen,$p); $p.Dispose()
    $p = [System.Drawing.Drawing2D.GraphicsPath]::new()
    $p.AddLine(192,156,192,180); $p.AddBezier(192,180,192,204,192,204,168,204); $p.AddLine(168,204,100,204)
    $g.DrawPath($pen,$p); $p.Dispose()
    $g.FillPolygon($mint,[System.Drawing.PointF[]]@([System.Drawing.PointF]::new(144,32),[System.Drawing.PointF]::new(172,52),[System.Drawing.PointF]::new(144,72)))
    $g.FillPolygon($mint,[System.Drawing.PointF[]]@([System.Drawing.PointF]::new(112,184),[System.Drawing.PointF]::new(84,204),[System.Drawing.PointF]::new(112,224)))
    foreach ($y in @(90,134)) {
        $p = RoundedPath 88 $y 80 32 8
        $g.FillPath($white,$p); $p.Dispose()
        $g.FillEllipse($navy,148,($y+12),8,8)
    }
    $g.Dispose(); $pen.Dispose(); $navy.Dispose(); $mint.Dispose(); $white.Dispose()
    $stream = [System.IO.MemoryStream]::new()
    $bmp.Save($stream,[System.Drawing.Imaging.ImageFormat]::Png)
    $images += ,@($size,$stream.ToArray())
    if ($size -eq 256) {
        [System.IO.File]::WriteAllBytes((Join-Path $assetDir 'icon.png'),$stream.ToArray())
        $rgba = [byte[]]::new(256*256*4)
        for ($y=0; $y -lt 256; $y++) { for ($x=0; $x -lt 256; $x++) {
            $c=$bmp.GetPixel($x,$y); $i=($y*256+$x)*4
            $rgba[$i]=$c.R; $rgba[$i+1]=$c.G; $rgba[$i+2]=$c.B; $rgba[$i+3]=$c.A
        }}
        [System.IO.File]::WriteAllBytes((Join-Path $assetDir 'icon.rgba'),$rgba)
    }
    $stream.Dispose(); $bmp.Dispose()
}
$file = [System.IO.File]::Create((Join-Path $assetDir 'hbsync.ico'))
$writer = [System.IO.BinaryWriter]::new($file)
$writer.Write([uint16]0); $writer.Write([uint16]1); $writer.Write([uint16]$images.Count)
$offset = 6+16*$images.Count
foreach ($entry in $images) {
    $s=$entry[0]; $bytes=$entry[1]
    $writer.Write([byte]($s % 256)); $writer.Write([byte]($s % 256))
    $writer.Write([byte]0); $writer.Write([byte]0)
    $writer.Write([uint16]1); $writer.Write([uint16]32)
    $writer.Write([uint32]$bytes.Length); $writer.Write([uint32]$offset)
    $offset += $bytes.Length
}
foreach ($entry in $images) { $writer.Write([byte[]]$entry[1]) }
$writer.Dispose()
Write-Output 'Generated SVG companion PNG, RGBA and nine-resolution Windows ICO.'
