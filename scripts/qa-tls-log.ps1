# QA do TLS puro no update: abre o app com um video, clica em "Check for updates"
# e mostra o log de sessao (linhas tls_provider= e event=update_check).
# ASCII-only (PowerShell 5.1).
$exe = Join-Path $PSScriptRoot '..\target\release\local-player-rs.exe'
$clip = Join-Path $PSScriptRoot '..\target\bench\clip.mkv'
if (-not (Test-Path $exe)) { throw "sem exe: $exe" }
if (-not (Test-Path $clip)) { throw "sem $clip" }
$logDir = Join-Path $env:APPDATA 'local-player-rs\logs'

Add-Type @"
using System;
using System.Runtime.InteropServices;
public static class Clk {
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] public static extern bool GetClientRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
    [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
    [DllImport("user32.dll")] public static extern void mouse_event(uint f, uint dx, uint dy, uint d, IntPtr e);
}
"@

$p = Start-Process -FilePath $exe -PassThru -ArgumentList $clip
try {
    Start-Sleep -Seconds 7
    $p.Refresh()
    $h = $p.MainWindowHandle
    if ($h -eq [IntPtr]::Zero) { throw 'janela nao apareceu' }
    [Clk]::SetForegroundWindow($h) | Out-Null
    Start-Sleep -Milliseconds 400
    $o = New-Object Clk+RECT; [Clk]::GetWindowRect($h, [ref]$o) | Out-Null
    $c = New-Object Clk+RECT; [Clk]::GetClientRect($h, [ref]$c) | Out-Null
    $cw = $c.Right - $c.Left
    $x = $o.Left + 16 + ($cw - 363)
    $y = $o.Top + 39 + 46
    [Clk]::SetCursorPos($x, $y) | Out-Null
    Start-Sleep -Milliseconds 300
    [Clk]::mouse_event(0x0002, 0, 0, 0, [IntPtr]::Zero)
    [Clk]::mouse_event(0x0004, 0, 0, 0, [IntPtr]::Zero)
    Start-Sleep -Seconds 6
} finally {
    try { $p.Kill() } catch { }
}

$newest = Get-ChildItem $logDir -Filter 'player-*.log' | Sort-Object LastWriteTime -Descending | Select-Object -First 1
if (-not $newest) { throw "sem log em $logDir" }
Write-Host ("log: {0}" -f $newest.FullName)
Get-Content $newest.FullName | Select-Object -First 8
