# QA objetivo do pipeline de 10 bits: o mesmo padrao estatico (SMPTE bars)
# codificado em 8 e 10 bits deve sair praticamente igual na tela. Captura a
# janela do player para cada arquivo e compara uma faixa central de pixels.
# Precisa das fixtures em C:\Dev\test-files\LocalMedia\10bit (ffmpeg).
# ASCII-only (PS 5.1 le script sem BOM como ANSI).
$ErrorActionPreference = 'Stop'
$exe = Join-Path $PSScriptRoot '..\target\release\local-player-rs.exe'
$fixtures = 'C:\Dev\test-files\LocalMedia\10bit'
$outdir = Join-Path $env:TEMP 'lp-qa'
New-Item -ItemType Directory -Force -Path $outdir | Out-Null

Add-Type -AssemblyName System.Drawing
Add-Type @"
using System;
using System.Runtime.InteropServices;
public class W2 {
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
}
"@

function Shot-Window([string]$media, [string]$png) {
  $proc = Start-Process -FilePath $exe -ArgumentList ('"' + $media + '"') -PassThru
  Start-Sleep -Seconds 5
  $proc.Refresh()
  $hwnd = $proc.MainWindowHandle
  if ($hwnd -eq [IntPtr]::Zero) { $proc | Stop-Process -Force; throw "janela nao encontrada para $media" }
  [void][W2]::SetForegroundWindow($hwnd)
  Start-Sleep -Milliseconds 600
  $r = New-Object W2+RECT
  [void][W2]::GetWindowRect($hwnd, [ref]$r)
  $w = $r.Right - $r.Left
  $h = $r.Bottom - $r.Top
  $bmp = New-Object System.Drawing.Bitmap $w, $h
  $g = [System.Drawing.Graphics]::FromImage($bmp)
  $g.CopyFromScreen($r.Left, $r.Top, 0, 0, $bmp.Size)
  $g.Dispose()
  $bmp.Save($png, [System.Drawing.Imaging.ImageFormat]::Png)
  $bmp.Dispose()
  $proc | Stop-Process -Force
  Start-Sleep -Milliseconds 500
  return @($w, $h)
}

function Sample-Strip([string]$png) {
  $bmp = [System.Drawing.Bitmap]::FromFile($png)
  $w = $bmp.Width
  $h = $bmp.Height
  if ($w -lt 400 -or $h -lt 400) {
    $bmp.Dispose()
    throw "captura pequena ($w x $h): a janela fechou o chrome ou nao pintou o video"
  }
  # Faixa central (25% de altura), de 20% a 80% da largura: sempre dentro do video
  # (letterbox de 16:9 numa janela ~1.42:1 fica com barras em cima/embaixo).
  $y0 = [int]($h * 0.42)
  $y1 = [int]($h * 0.58)
  $x0 = [int]($w * 0.20)
  $x1 = [int]($w * 0.80)
  $sum = @(0.0, 0.0, 0.0)
  $n = 0
  $rows = @()
  for ($y = $y0; $y -le $y1; $y += 7) {
    for ($x = $x0; $x -le $x1; $x += 7) {
      $c = $bmp.GetPixel($x, $y)
      $sum[0] += $c.R; $sum[1] += $c.G; $sum[2] += $c.B
      $rows += , @($x, $y, $c.R, $c.G, $c.B)
      $n++
    }
  }
  $bmp.Dispose()
  return @{ w = $w; h = $h; n = $n; mean = @(($sum[0] / $n), ($sum[1] / $n), ($sum[2] / $n)); px = $rows }
}

$media8 = Join-Path $fixtures 'bars-8bit.mp4'
$media10 = Join-Path $fixtures 'bars-10bit.mp4'
if (-not (Test-Path $media8) -or -not (Test-Path $media10)) { throw "sem fixtures bars-8bit/bars-10bit em $fixtures" }
$p8 = Join-Path $outdir 'bars-8bit.png'
$p10 = Join-Path $outdir 'bars-10bit.png'

$size8 = Shot-Window $media8 $p8
$size10 = Shot-Window $media10 $p10
Write-Output ("janela 8-bit: {0}x{1} / 10-bit: {2}x{3}" -f $size8[0], $size8[1], $size10[0], $size10[1])

$a = Sample-Strip $p8
$b = Sample-Strip $p10
if ($a.w -ne $b.w -or $a.h -ne $b.h) {
  $msg = 'janelas de tamanhos diferentes (8-bit {0}x{1} x 10-bit {2}x{3}): repetir a captura' -f $a.w, $a.h, $b.w, $b.h
  throw $msg
}
Write-Output ("amostras: {0} x {1}" -f $a.n, $b.n)
Write-Output ("media 8-bit : R={0:N1} G={1:N1} B={2:N1}" -f $a.mean[0], $a.mean[1], $a.mean[2])
Write-Output ("media 10-bit: R={0:N1} G={1:N1} B={2:N1}" -f $b.mean[0], $b.mean[1], $b.mean[2])
$sumAbs = 0.0
$worst = 0
for ($i = 0; $i -lt [Math]::Min($a.px.Count, $b.px.Count); $i++) {
  $pa = $a.px[$i]; $pb = $b.px[$i]
  foreach ($k in 2, 3, 4) {
    $d = [Math]::Abs($pa[$k] - $pb[$k])
    $sumAbs += $d
    if ($d -gt $worst) { $worst = $d }
  }
}
$meanAbs = $sumAbs / ([Math]::Min($a.px.Count, $b.px.Count) * 3)
Write-Output ("delta medio por canal: {0:N2} / pior: {1}" -f $meanAbs, $worst)
