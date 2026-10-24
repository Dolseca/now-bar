# DPI-aware screen capture of a region. Example: pwsh tools/shot.ps1 -Out pill.png -X 610 -Y 1010 -W 700 -H 70
param([Parameter(Mandatory)][string]$Out, [int]$X, [int]$Y, [int]$W = 700, [int]$H = 330, [int]$Zoom = 1)
Add-Type @"
using System; using System.Runtime.InteropServices;
public static class ShotDpi { [DllImport("user32.dll")] public static extern bool SetProcessDpiAwarenessContext(IntPtr v); }
"@
[ShotDpi]::SetProcessDpiAwarenessContext([IntPtr]-4) | Out-Null
Add-Type -AssemblyName System.Drawing
$bmp = New-Object System.Drawing.Bitmap $W, $H
$g = [System.Drawing.Graphics]::FromImage($bmp)
$g.CopyFromScreen($X, $Y, 0, 0, $bmp.Size)
if ($Zoom -gt 1) {
  $big = New-Object System.Drawing.Bitmap ($W * $Zoom), ($H * $Zoom)
  $g2 = [System.Drawing.Graphics]::FromImage($big)
  $g2.InterpolationMode = 'NearestNeighbor'; $g2.PixelOffsetMode = 'Half'
  $g2.DrawImage($bmp, 0, 0, $W * $Zoom, $H * $Zoom)
  $bmp = $big
}
$bmp.Save($Out, [System.Drawing.Imaging.ImageFormat]::Png)
$Out
