# QA: abre um arquivo, aperta D (overlay) e captura a janela. Parametros:
#   -Media <arquivo>  -Out <png>  [-Tonemap bt2390|hable]
# ASCII-only (PS 5.1 le script sem BOM como ANSI).
param(
  [Parameter(Mandatory = $true)][string]$Media,
  [Parameter(Mandatory = $true)][string]$Out,
  [string]$Tonemap = '',
  [string]$Exe = ''
)
$ErrorActionPreference = 'Stop'
$exe = if ($Exe -ne '') { $Exe } else { Join-Path $PSScriptRoot '..\target\release\local-player-rs.exe' }
if (-not (Test-Path $exe)) { throw "sem binario $exe (cargo build --release)" }
$json = Join-Path $env:APPDATA 'local-player-rs\playlist.json'

Add-Type -AssemblyName System.Drawing
Add-Type -AssemblyName System.Windows.Forms
Add-Type @"
using System;
using System.Runtime.InteropServices;
public class W3 {
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
  [DllImport("user32.dll")] public static extern void mouse_event(uint f, uint dx, uint dy, uint d, UIntPtr e);
}
"@

# playlist limpa: o painel volta ao modo automatico e nao cobre o overlay.
if (Test-Path $json) { Remove-Item $json -Force }
if ($Tonemap -ne '') { $env:LOCALPLAYER_HDR_TONEMAP = $Tonemap } else { Remove-Item Env:LOCALPLAYER_HDR_TONEMAP -ErrorAction SilentlyContinue }

$proc = Start-Process -FilePath $exe -ArgumentList ('"' + $Media + '"') -PassThru
Start-Sleep -Seconds 5
$proc.Refresh()
$hwnd = $proc.MainWindowHandle
if ($hwnd -eq [IntPtr]::Zero) { $proc | Stop-Process -Force; throw 'janela nao encontrada' }
[void][W3]::SetForegroundWindow($hwnd)
Start-Sleep -Milliseconds 300
$shell = New-Object -ComObject WScript.Shell
[void]$shell.AppActivate($proc.Id)
Start-Sleep -Milliseconds 400

$r = New-Object W3+RECT
[void][W3]::GetWindowRect($hwnd, [ref]$r)
# Um clique no video garante foco (SetForegroundWindow pode ser recusado) e
# faz o chrome reaparecer para a captura.
[void][W3]::SetCursorPos($r.Left + [int](($r.Right - $r.Left) / 2), $r.Top + [int](($r.Bottom - $r.Top) / 2))
Start-Sleep -Milliseconds 120
[W3]::mouse_event(0x0002, 0, 0, 0, [UIntPtr]::Zero)
Start-Sleep -Milliseconds 80
[W3]::mouse_event(0x0004, 0, 0, 0, [UIntPtr]::Zero)
Start-Sleep -Milliseconds 300
[System.Windows.Forms.SendKeys]::SendWait('d')
Start-Sleep -Milliseconds 900

$r = New-Object W3+RECT
[void][W3]::GetWindowRect($hwnd, [ref]$r)
$w = $r.Right - $r.Left
$h = $r.Bottom - $r.Top
$bmp = New-Object System.Drawing.Bitmap $w, $h
$g = [System.Drawing.Graphics]::FromImage($bmp)
$g.CopyFromScreen($r.Left, $r.Top, 0, 0, $bmp.Size)
$g.Dispose()
$bmp.Save($Out, [System.Drawing.Imaging.ImageFormat]::Png)
$bmp.Dispose()
$proc | Stop-Process -Force
Write-Output ("capturado {0} ({1}x{2}) tonemap='{3}'" -f $Out, $w, $h, $Tonemap)
