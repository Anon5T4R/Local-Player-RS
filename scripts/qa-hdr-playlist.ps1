# QA manual do local-player-rs: overlay D (tecla D) + resize da playlist.
# Precisa das fixtures HEVC Main 10 em C:\Dev\test-files\LocalMedia\10bit
# (geradas por ffmpeg; ver relatorio da v0.6.0).
# ASCII-only (PS 5.1 le script sem BOM como ANSI).
$ErrorActionPreference = 'Stop'

$exe = Join-Path $PSScriptRoot '..\target\release\local-player-rs.exe'
$fixtures = 'C:\Dev\test-files\LocalMedia\10bit'
$media = Join-Path $fixtures 'hdr10-720p-main10.mp4'
if (-not (Test-Path $media)) { throw "sem fixture $media" }
$outdir = Join-Path $env:TEMP 'lp-qa'
New-Item -ItemType Directory -Force -Path $outdir | Out-Null
$csv = Join-Path $outdir 'stats.csv'
$json = Join-Path $env:APPDATA 'local-player-rs\playlist.json'

Add-Type -AssemblyName System.Drawing
Add-Type -AssemblyName System.Windows.Forms
Add-Type @"
using System;
using System.Runtime.InteropServices;
public class W {
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
  [StructLayout(LayoutKind.Sequential)] public struct POINT { public int X, Y; }
  [StructLayout(LayoutKind.Sequential)] public struct CURSORINFO { public int cbSize; public int flags; public IntPtr hCursor; public POINT pt; }
  [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
  [DllImport("user32.dll")] public static extern void mouse_event(uint f, uint dx, uint dy, uint d, UIntPtr e);
  [DllImport("user32.dll")] public static extern bool GetCursorInfo(ref CURSORINFO pci);
  [DllImport("user32.dll")] public static extern IntPtr LoadCursor(IntPtr h, IntPtr name);
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool GetClientRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool ClientToScreen(IntPtr h, ref POINT p);
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern IntPtr FindWindow(string cls, string title);
}
"@

function Cursor-Handle {
  $ci = New-Object W+CURSORINFO
  $ci.cbSize = [System.Runtime.InteropServices.Marshal]::SizeOf($ci)
  if (-not [W]::GetCursorInfo([ref]$ci)) { return [IntPtr]::Zero }
  return $ci.hCursor
}

function Shot([string]$path, [int]$l, [int]$t, [int]$w, [int]$h) {
  $bmp = New-Object System.Drawing.Bitmap $w, $h
  $g = [System.Drawing.Graphics]::FromImage($bmp)
  $g.CopyFromScreen($l, $t, 0, 0, $bmp.Size)
  $g.Dispose()
  $bmp.Save($path, [System.Drawing.Imaging.ImageFormat]::Png)
  $bmp.Dispose()
}

# Remove a playlist salva para o painel comecar no modo automatico.
if (Test-Path $json) { Remove-Item $json -Force }

$env:LOCALPLAYER_STATS = $csv
$proc = Start-Process -FilePath $exe -ArgumentList @($media) -PassThru
Start-Sleep -Seconds 6
$proc.Refresh()
$hwnd = $proc.MainWindowHandle
if ($hwnd -eq [IntPtr]::Zero) { $hwnd = [W]::FindWindow($null, 'Local Player RS') }
if ($hwnd -eq [IntPtr]::Zero) { Write-Output 'FALHA: janela nao encontrada'; $proc | Stop-Process -Force; exit 1 }
[void][W]::SetForegroundWindow($hwnd)
Start-Sleep -Milliseconds 400

$r = New-Object W+RECT
[void][W]::GetWindowRect($hwnd, [ref]$r)
$w = $r.Right - $r.Left
$h = $r.Bottom - $r.Top
Write-Output ("janela: {0},{1} {2}x{3}" -f $r.Left, $r.Top, $w, $h)

# Overlay D.
[System.Windows.Forms.SendKeys]::SendWait('d')
Start-Sleep -Milliseconds 800
Shot (Join-Path $outdir 'overlay-10bit.png') $r.Left $r.Top $w $h

# Procura a borda de resize da playlist: o egui poe o cursor IDC_SIZENS (32645)
# sobre a alca; varremos de baixo para cima DENTRO do cliente (a borda nativa da
# janela tambem e IDC_SIZENS e nao pode ser confundida com a alca).
$sizens = [W]::LoadCursor([IntPtr]::Zero, [IntPtr]32645)
$cr = New-Object W+RECT
[void][W]::GetClientRect($hwnd, [ref]$cr)
$origin = New-Object W+POINT
$origin.X = 0; $origin.Y = 0
[void][W]::ClientToScreen($hwnd, [ref]$origin)
$clientBottom = $origin.Y + $cr.Bottom
$cx = $origin.X + [int](($cr.Right - $cr.Left) / 2)
[void][W]::SetCursorPos($cx, $origin.Y + 40)
Start-Sleep -Milliseconds 90
$plain = Cursor-Handle
Write-Output ("cursor no video: {0} (sizens={1})" -f $plain, $sizens)
$edge = -1
for ($y = $clientBottom - 14; $y -ge $clientBottom - 320; $y -= 2) {
  [void][W]::SetCursorPos($cx, $y)
  Start-Sleep -Milliseconds 45
  if ((Cursor-Handle) -eq $sizens) { $edge = $y; break }
}
Write-Output ("borda de resize em y={0} (cliente termina em {1})" -f $edge, $clientBottom)

if ($edge -gt 0) {
  # Arrasta para cima 140 px (painel cresce).
  [void][W]::SetCursorPos($cx, $edge)
  Start-Sleep -Milliseconds 120
  [W]::mouse_event(0x0002, 0, 0, 0, [UIntPtr]::Zero)
  Start-Sleep -Milliseconds 120
  for ($i = 1; $i -le 14; $i++) {
    [void][W]::SetCursorPos($cx, $edge - (10 * $i))
    Start-Sleep -Milliseconds 40
  }
  [W]::mouse_event(0x0004, 0, 0, 0, [UIntPtr]::Zero)
  Start-Sleep -Milliseconds 700
  Shot (Join-Path $outdir 'playlist-cresceu.png') $r.Left $r.Top $w $h
  Start-Sleep -Milliseconds 300
  if (Test-Path $json) { Write-Output ("playlist.json: " + (Get-Content $json -Raw)) } else { Write-Output 'playlist.json NAO gravado' }

  # Duplo-clique na alca volta para a altura automatica (segue o conteudo).
  # A janela perde foco para o console durante a captura, e sem foco o winit nao
  # entrega movimento do mouse: clica no titulo (refoca) e varre rapido (o chrome
  # some 2,5 s depois da ultima atividade).
  [void][W]::SetCursorPos($cx, $r.Top + 12)
  Start-Sleep -Milliseconds 80
  [W]::mouse_event(0x0002, 0, 0, 0, [UIntPtr]::Zero)
  Start-Sleep -Milliseconds 40
  [W]::mouse_event(0x0004, 0, 0, 0, [UIntPtr]::Zero)
  Start-Sleep -Milliseconds 200
  $edge2 = -1
  for ($y = $clientBottom - 160; $y -ge $clientBottom - 320; $y -= 3) {
    [void][W]::SetCursorPos($cx, $y)
    Start-Sleep -Milliseconds 40
    if ((Cursor-Handle) -eq $sizens) { $edge2 = $y; break }
  }
  Write-Output ("alca depois do resize em y={0}" -f $edge2)
  if ($edge2 -gt 0) {
    for ($k = 1; $k -le 2; $k++) {
      [W]::mouse_event(0x0002, 0, 0, 0, [UIntPtr]::Zero)
      Start-Sleep -Milliseconds 60
      [W]::mouse_event(0x0004, 0, 0, 0, [UIntPtr]::Zero)
      Start-Sleep -Milliseconds 60
    }
    Start-Sleep -Milliseconds 800
    if (Test-Path $json) { Write-Output ("playlist.json apos duplo-clique: " + (Get-Content $json -Raw)) }
    Shot (Join-Path $outdir 'playlist-auto.png') $r.Left $r.Top $w $h
  }
}

Start-Sleep -Seconds 2
$proc | Stop-Process -Force
Start-Sleep -Milliseconds 500
if (Test-Path $csv) { Write-Output 'CSV (ultimas linhas):'; Get-Content $csv | Select-Object -Last 4 }
