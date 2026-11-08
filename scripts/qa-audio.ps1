# QA: audio puro NAO mexe na janela (o resize 1:1 e so de video).
# ASCII-only (PowerShell 5.1).
$exe = Join-Path $PSScriptRoot '..\target\release\local-player-rs.exe'
$mp3 = Join-Path $PSScriptRoot '..\target\bench\clip.mp3'
if (-not (Test-Path $mp3)) { throw 'sem target\bench\clip.mp3' }

Add-Type @"
using System;
using System.Runtime.InteropServices;
public static class Rect2 {
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
    [DllImport("user32.dll")] public static extern bool GetClientRect(IntPtr h, out RECT r);
}
"@

$p = Start-Process -FilePath $exe -PassThru -ArgumentList $mp3
try {
    Start-Sleep -Seconds 5
    $p.Refresh()
    $r = New-Object Rect2+RECT
    [Rect2]::GetClientRect($p.MainWindowHandle, [ref]$r) | Out-Null
    Write-Host ("cliente (audio puro) = {0}x{1}" -f ($r.Right - $r.Left), ($r.Bottom - $r.Top))
} finally {
    try { $p.Kill() } catch { }
}
