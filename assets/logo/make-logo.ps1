# Builds the Orbit logo set: SVG masters, pixel-snapped PNGs and a multi-size ICO.
# Mark: 5x5 pixel grid. Ring (white) + core (blue, centre) + satellite (cyan, top right).
# Usage: powershell -ExecutionPolicy Bypass -File make-logo.ps1
Add-Type -AssemblyName System.Drawing
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$root = Split-Path -Parent (Split-Path -Parent $here)   # orbit/

$ring = @(@(1,0),@(2,0),@(3,0),@(0,1),@(0,2),@(0,3),@(4,1),@(4,2),@(4,3),@(1,4),@(2,4),@(3,4))
$cells = @()
foreach ($p in $ring) { $cells += ,@($p[0], $p[1], 'w') }
$cells += ,@(2, 2, 'b')
$cells += ,@(4, 0, 'c')

$dark  = @{ bg = '#0A0F1F'; w = '#F2F5FF'; b = '#4F8CFF'; c = '#38D5F0' }
$light = @{ bg = '#F4F6FB'; w = '#0A0F1F'; b = '#2F6FEB'; c = '#0B8DB8' }

function Svg($t, $tile, $title) {
  # tile: navy square, grid 15 + 14*i, blocks 11 (matches the LegacyTech icon proportions)
  # mark: transparent, grid 5 + 21*i, blocks 16 in a 110 box
  if ($tile) { $vb = 100; $o = 15; $st = 14; $bs = 11 } else { $vb = 110; $o = 5; $st = 21; $bs = 16 }
  $s = "<svg xmlns=`"http://www.w3.org/2000/svg`" viewBox=`"0 0 $vb $vb`" width=`"512`" height=`"512`" role=`"img`"><title>$title</title>"
  if ($tile) { $s += "<rect width=`"$vb`" height=`"$vb`" fill=`"$($t.bg)`"/>" }
  foreach ($k in 'w','b','c') {
    $d = ($cells | Where-Object { $_[2] -eq $k } | ForEach-Object { "M$($o + $st*$_[0]) $($o + $st*$_[1])h${bs}v${bs}h-${bs}z" }) -join ' '
    $s += "<path fill=`"$($t[$k])`" d=`"$d`"/>"
  }
  return $s + '</svg>'
}

$svgs = @{
  'orbit-icon.svg'            = (Svg $dark  $true  'Orbit icon')
  'orbit-icon-light.svg'      = (Svg $light $true  'Orbit icon light')
  'orbit-mark-color-dark.svg' = (Svg $dark  $false 'Orbit mark color-dark')
  'orbit-mark-color-light.svg'= (Svg $light $false 'Orbit mark color-light')
}
foreach ($n in $svgs.Keys) { [IO.File]::WriteAllText("$here\$n", $svgs[$n]) }

function Png($size, $t, $path) {
  # Pixel-snapped geometry so small sizes stay sharp.
  switch ($size) {
    16 { $bs = 2; $gap = 1 }
    32 { $bs = 4; $gap = 2 }
    48 { $bs = 6; $gap = 2 }
    default { $bs = [int][math]::Round($size * 0.11); $gap = [int][math]::Round($size * 0.03) }
  }
  $span = 5 * $bs + 4 * $gap
  $o = [int][math]::Floor(($size - $span) / 2)
  $bmp = New-Object System.Drawing.Bitmap $size, $size, ([System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
  $g = [System.Drawing.Graphics]::FromImage($bmp)
  $g.SmoothingMode = 'None'; $g.PixelOffsetMode = 'Half'
  $g.Clear([System.Drawing.ColorTranslator]::FromHtml($t.bg))
  foreach ($c in $cells) {
    $br = New-Object System.Drawing.SolidBrush ([System.Drawing.ColorTranslator]::FromHtml($t[$c[2]]))
    $g.FillRectangle($br, $o + ($bs + $gap) * $c[0], $o + ($bs + $gap) * $c[1], $bs, $bs)
    $br.Dispose()
  }
  $g.Dispose(); $bmp.Save($path, [System.Drawing.Imaging.ImageFormat]::Png); $bmp.Dispose()
}

$sizes = 16, 32, 48, 64, 128, 256, 512
foreach ($s in $sizes) { Png $s $dark "$here\orbit-icon-$s.png" }
Png 512 $light "$here\orbit-icon-light-512.png"

# ICO with PNG entries (16..256)
$icoSizes = 16, 32, 48, 64, 128, 256
$data = $icoSizes | ForEach-Object { ,[IO.File]::ReadAllBytes("$here\orbit-icon-$_.png") }
$ms = New-Object IO.MemoryStream; $bw = New-Object IO.BinaryWriter($ms)
$bw.Write([UInt16]0); $bw.Write([UInt16]1); $bw.Write([UInt16]$icoSizes.Count)
$off = 6 + 16 * $icoSizes.Count
for ($i = 0; $i -lt $icoSizes.Count; $i++) {
  $b = if ($icoSizes[$i] -ge 256) { 0 } else { $icoSizes[$i] }
  $bw.Write([byte]$b); $bw.Write([byte]$b); $bw.Write([byte]0); $bw.Write([byte]0)
  $bw.Write([UInt16]1); $bw.Write([UInt16]32); $bw.Write([UInt32]$data[$i].Length); $bw.Write([UInt32]$off)
  $off += $data[$i].Length
}
foreach ($d in $data) { $bw.Write($d) }
$bw.Flush(); [IO.File]::WriteAllBytes("$here\orbit.ico", $ms.ToArray())

# Install into the app
Copy-Item "$here\orbit.ico" "$root\src-tauri\icons\icon.ico" -Force
Copy-Item "$here\orbit-icon-512.png" "$root\src-tauri\icons\icon.png" -Force
Copy-Item "$here\orbit-mark-color-dark.svg" "$root\ui\assets\orbit-mark-dark.svg" -Force
Copy-Item "$here\orbit-mark-color-light.svg" "$root\ui\assets\orbit-mark-light.svg" -Force
"Logo set written to $here and installed into the app."
