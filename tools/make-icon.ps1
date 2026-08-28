Add-Type -AssemblyName System.Drawing
$size = 1024
$bmp = New-Object System.Drawing.Bitmap($size, $size)
$g = [System.Drawing.Graphics]::FromImage($bmp)
$g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias

$g.Clear([System.Drawing.Color]::Transparent)

# rounded-square dark background
$r = 230
$path = New-Object System.Drawing.Drawing2D.GraphicsPath
$path.AddArc(0, 0, $r, $r, 180, 90)
$path.AddArc($size - $r, 0, $r, $r, 270, 90)
$path.AddArc($size - $r, $size - $r, $r, $r, 0, 90)
$path.AddArc(0, $size - $r, $r, $r, 90, 90)
$path.CloseFigure()
$bgBrush = New-Object System.Drawing.Drawing2D.LinearGradientBrush(
    (New-Object System.Drawing.Point(0, 0)),
    (New-Object System.Drawing.Point($size, $size)),
    [System.Drawing.Color]::FromArgb(255, 13, 22, 38),
    [System.Drawing.Color]::FromArgb(255, 26, 39, 64))
$g.FillPath($bgBrush, $path)

# gauge ring: track + 75% emerald arc
$cx = 512; $cy = 512; $rad = 300; $penW = 88
$rect = New-Object System.Drawing.Rectangle(($cx - $rad), ($cy - $rad), (2 * $rad), (2 * $rad))
$track = New-Object System.Drawing.Pen([System.Drawing.Color]::FromArgb(120, 148, 163, 184), $penW)
$g.DrawArc($track, $rect, 0, 360)
$pen = New-Object System.Drawing.Pen([System.Drawing.Color]::FromArgb(255, 52, 211, 153), $penW)
$pen.StartCap = [System.Drawing.Drawing2D.LineCap]::Round
$pen.EndCap = [System.Drawing.Drawing2D.LineCap]::Round
$g.DrawArc($pen, $rect, -90, 270)

# center: up (green) + down (sky) triangles = traffic in/out
$tri = 64
$greenBrush = New-Object System.Drawing.SolidBrush([System.Drawing.Color]::FromArgb(255, 52, 211, 153))
$skyBrush = New-Object System.Drawing.SolidBrush([System.Drawing.Color]::FromArgb(255, 56, 189, 248))

$upPts = [System.Drawing.PointF[]]@(
    (New-Object System.Drawing.PointF(($cx - $tri), ($cy - 46))),
    (New-Object System.Drawing.PointF(($cx + $tri), ($cy - 46))),
    (New-Object System.Drawing.PointF($cx, ($cy - 46 - $tri)))
)
$g.FillPolygon($greenBrush, $upPts)

$downPts = [System.Drawing.PointF[]]@(
    (New-Object System.Drawing.PointF(($cx - $tri), ($cy + 46))),
    (New-Object System.Drawing.PointF(($cx + $tri), ($cy + 46))),
    (New-Object System.Drawing.PointF($cx, ($cy + 46 + $tri)))
)
$g.FillPolygon($skyBrush, $downPts)

$g.Dispose()
$bmp.Save("C:\Users\yuki\Documents\Code\Translator\tools\app-icon.png", [System.Drawing.Imaging.ImageFormat]::Png)
$bmp.Dispose()
Write-Output "icon written"
