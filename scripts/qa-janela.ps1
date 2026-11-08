# QA real do resize 1:1: abre o app com o MKV de teste, espera o primeiro frame,
# mede a janela (cliente/outer/DPI) e salva um screenshot.
# ASCII-only (PowerShell 5.1 le script sem BOM como ANSI).
# Uso: powershell -ExecutionPolicy Bypass -File scripts\qa-janela.ps1

$ErrorActionPreference = 'Stop'

$exe  = Join-Path $PSScriptRoot '..\target\release\local-player-rs.exe'
# Caminho ASCII: o nome original tem japones e o -ArgumentList nao cita sozinho.
$clip = Join-Path $PSScriptRoot '..\target\bench\clip.mkv'
$file = (Get-ChildItem 'C:\Dev\test-files\LocalMedia' -Filter 'Ado*.mkv' | Select-Object -First 1).FullName
if (-not (Test-Path $exe))  { throw "sem exe: $exe" }
if (-not $file) { throw 'sem Ado*.mkv em C:\Dev\test-files\LocalMedia' }
New-Item -ItemType Directory -Force -Path (Split-Path $clip) | Out-Null
if (-not (Test-Path $clip)) { Copy-Item $file $clip }

$shot = Join-Path $PSScriptRoot '..\target\qa-janela.png'

Add-Type -AssemblyName System.Windows.Forms, System.Drawing
Add-Type @"
using System;
using System.Runtime.InteropServices;
public static class Win {
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] public static extern bool GetClientRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] public static extern uint GetDpiForWindow(IntPtr h);
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
}
"@

$p = Start-Process -FilePath $exe -PassThru -ArgumentList $clip
try {
    # Motor abre a faixa numa thread; o primeiro frame e o resize levam ~1-2 s.
    Start-Sleep -Seconds 6
    $p.Refresh()
    $h = $p.MainWindowHandle
    if ($h -eq [IntPtr]::Zero) { throw 'janela nao apareceu' }
    [Win]::SetForegroundWindow($h) | Out-Null
    # Mouse para dentro da janela: os controles somem com 2,5 s de inatividade, e
    # o chrome tem que estar VISIVEL para medir a area do video.
    $probe = New-Object Win+RECT
    [Win]::GetWindowRect($h, [ref]$probe) | Out-Null
    [System.Windows.Forms.Cursor]::Position = New-Object System.Drawing.Point ($probe.Left + 60), ($probe.Top + 60)
    Start-Sleep -Milliseconds 500

    $outer = New-Object Win+RECT
    $client = New-Object Win+RECT
    [Win]::GetWindowRect($h, [ref]$outer) | Out-Null
    [Win]::GetClientRect($h, [ref]$client) | Out-Null
    $dpi = [Win]::GetDpiForWindow($h)
    $scale = $dpi / 96.0
    Write-Host ("janela outer = {0}x{1}" -f ($outer.Right - $outer.Left), ($outer.Bottom - $outer.Top))
    Write-Host ("janela cliente (px fisicos) = {0}x{1}" -f ($client.Right - $client.Left), ($client.Bottom - $client.Top))
    Write-Host ("dpi = {0} (escala {1})" -f $dpi, $scale)

    $w = $outer.Right - $outer.Left
    $hh = $outer.Bottom - $outer.Top
    $bmp = New-Object System.Drawing.Bitmap $w, $hh
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.CopyFromScreen($outer.Left, $outer.Top, 0, 0, (New-Object System.Drawing.Size $w, $hh))
    $bmp.Save($shot, [System.Drawing.Imaging.ImageFormat]::Png)
    $g.Dispose(); $bmp.Dispose()
    Write-Host "screenshot: $shot"
} finally {
    try { $p.Kill() } catch { }
}
