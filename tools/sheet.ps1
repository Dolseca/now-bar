# Tiles PNG frames (sorted by name) from -Dir into one contact sheet, -Cols wide, scaled by -Scale.
param([Parameter(Mandatory)][string]$Dir, [Parameter(Mandatory)][string]$Out, [int]$From = 0, [int]$Count = 12, [int]$Cols = 3, [double]$Scale = 0.5)
Add-Type -AssemblyName System.Drawing
$files = Get-ChildItem $Dir -Filter *.png | Sort-Object Name | Select-Object -Skip $From -First $Count
$first = [System.Drawing.Image]::FromFile($files[0].FullName)
$tw = [int]($first.Width * $Scale); $th = [int]($first.Height * $Scale); $first.Dispose()
$rows = [math]::Ceiling($files.Count / $Cols)
$sheet = New-Object System.Drawing.Bitmap ($tw * $Cols), (($th + 16) * $rows)
$g = [System.Drawing.Graphics]::FromImage($sheet); $g.Clear([System.Drawing.Color]::White)
$font = New-Object System.Drawing.Font "Segoe UI", 9
for ($i = 0; $i -lt $files.Count; $i++) {
  $img = [System.Drawing.Image]::FromFile($files[$i].FullName)
  $x = ($i % $Cols) * $tw; $y = [math]::Floor($i / $Cols) * ($th + 16)
  $g.DrawImage($img, $x, $y + 16, $tw, $th); $img.Dispose()
  $g.DrawString($files[$i].BaseName, $font, [System.Drawing.Brushes]::Black, $x + 2, $y)
}
$sheet.Save($Out, [System.Drawing.Imaging.ImageFormat]::Png); $Out
