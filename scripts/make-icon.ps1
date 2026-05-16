# Generates a 1024x1024 placeholder PNG for Tauri's icon pipeline.
Add-Type -AssemblyName System.Drawing
$bmp = New-Object System.Drawing.Bitmap 1024, 1024
$g = [System.Drawing.Graphics]::FromImage($bmp)
$g.SmoothingMode = 'AntiAlias'
$g.Clear([System.Drawing.Color]::FromArgb(12, 12, 12))
$brush = New-Object System.Drawing.SolidBrush([System.Drawing.Color]::FromArgb(0, 153, 255))
$g.FillRectangle($brush, 320, 256, 384, 512)
$whiteBrush = [System.Drawing.Brushes]::White
$font = New-Object System.Drawing.Font('Arial', 320.0, [System.Drawing.FontStyle]::Bold)
$sf = New-Object System.Drawing.StringFormat
$sf.Alignment = 1
$sf.LineAlignment = 1
$rect = New-Object System.Drawing.RectangleF -ArgumentList 0.0, 0.0, 1024.0, 1024.0
$g.DrawString('I', $font, $whiteBrush, $rect, $sf)
$out = (Resolve-Path .).Path + '\icon-source.png'
$bmp.Save($out, [System.Drawing.Imaging.ImageFormat]::Png)
$g.Dispose()
$bmp.Dispose()
Write-Output "wrote $out"
