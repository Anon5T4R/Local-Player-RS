<#
.SYNOPSIS
  Build and package Local Player RS for Windows.

.DESCRIPTION
  Produces, in $env:DIST (default: dist\release):
    local-player-rs-<version>-windows-x64.msi              per-machine installer (WiX v5)
    local-player-rs-<version>-windows-x64-portable.zip     local-player-rs.exe + README

  The asset names follow the suite's pattern the updater (craft_update) parses:
  <app>-<version>-<os>-<arch>[-portable].<ext>. The binary links the C runtime
  statically (+crt-static), so neither the MSI nor the portable zip needs the
  Visual C++ redistributable. No signing (the Local suite's releases are
  unsigned; the updater verifies downloads against GitHub's per-asset SHA-256).
  Nothing is downloaded at runtime: the decoder is pure Rust inside the binary.

  Needs: Rust (MSVC toolchain + the x86_64-pc-windows-msvc target) and WiX v5
  with its UI and Util extensions (installer dialogs, "Launch Local Player" on
  finish):
    dotnet tool install --global wix --version 5.0.2
    wix extension add -g WixToolset.UI.wixext/5.0.2
    wix extension add -g WixToolset.Util.wixext/5.0.2

.EXAMPLE
  pwsh packaging/windows/package.ps1
  pwsh packaging/windows/package.ps1 -SkipBuild
#>
param(
  [switch] $SkipBuild
)
$ErrorActionPreference = 'Stop'
$Root = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path

function Invoke-Native([string] $What, [scriptblock] $Block) {
  Write-Output "==> $What"
  & $Block
  if ($LASTEXITCODE -ne 0) { throw "$What failed with exit code $LASTEXITCODE" }
}

# The version lives in one place: [package] version in the root Cargo.toml.
$Version = $env:LOCALPLAYER_RS_VERSION
if (-not $Version) {
  $inPkg = $false
  foreach ($line in Get-Content (Join-Path $Root 'Cargo.toml')) {
    if ($line -match '^\s*\[') { $inPkg = ($line.Trim() -eq '[package]'); continue }
    if ($inPkg -and $line -match '^\s*version\s*=\s*"([^"]+)"') { $Version = $Matches[1]; break }
  }
}
if (-not $Version) { throw 'could not read [package] version from Cargo.toml' }
# MSI ProductVersion is numeric (major.minor.build); pre-release tags are dropped there.
$MsiVersion = ($Version -split '-')[0]

$Arch = 'x64'
$Target = 'x86_64-pc-windows-msvc'
$Dist = if ($env:DIST) { $env:DIST } else { Join-Path $Root 'dist\release' }
$TargetDir = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $Root 'target' }
New-Item -ItemType Directory -Force -Path $Dist | Out-Null

Write-Output "Local Player RS $Version for Windows $Arch ($Target)"

if (-not $SkipBuild) {
  # Static CRT: no VC++ redistributable needed. Scoped to the target so host
  # build scripts and proc-macros are unaffected.
  $env:CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUSTFLAGS = '-C target-feature=+crt-static'
  Invoke-Native "cargo build ($Target)" { cargo build --release --locked --bin local-player-rs --target $Target }
}

$Bin = Join-Path $TargetDir "$Target\release"

# Check the exe's PE header before packaging. Machine (COFF header) must be
# x64, so an x64 build can never ship labelled otherwise. Subsystem (optional
# header): 2 = Windows GUI — no console window opens with the app.
function Get-PeHeader([string] $Path) {
  $bytes = [System.IO.File]::ReadAllBytes($Path)
  $pe = [BitConverter]::ToInt32($bytes, 0x3C)
  return @{ Machine = [BitConverter]::ToUInt16($bytes, $pe + 4); Subsystem = [BitConverter]::ToUInt16($bytes, $pe + 0x5C) }
}
$h = Get-PeHeader (Join-Path $Bin 'local-player-rs.exe')
if ($h.Machine -ne 0x8664) { throw "local-player-rs.exe is for machine 0x$('{0:X}' -f $h.Machine), expected 0x8664 ($Arch)" }
if ($h.Subsystem -ne 2) { throw "local-player-rs.exe has PE subsystem $($h.Subsystem), expected 2 (GUI)" }
Write-Output "ok local-player-rs.exe: $Arch, PE subsystem $($h.Subsystem)"

$Stage = Join-Path $TargetDir "windows-package\$Arch"
Remove-Item -Recurse -Force $Stage -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force -Path $Stage | Out-Null
Copy-Item (Join-Path $Bin 'local-player-rs.exe') $Stage

# ---- MSI ---------------------------------------------------------------------------------------
# The installer's welcome page shows the license. The repo carries no LICENSE
# file (the crate declares GPL-3.0-or-later), so the RTF is a short notice
# pointing at the full text, hard-wrapped for the dialog's width.
function ConvertTo-RtfText([string] $Text) {
  $sb = [System.Text.StringBuilder]::new()
  foreach ($ch in $Text.ToCharArray()) {
    $c = [int] $ch
    if ($c -eq 0x5C -or $c -eq 0x7B -or $c -eq 0x7D) { [void] $sb.Append('\').Append($ch) }
    elseif ($c -gt 127) { [void] $sb.Append("\u$(if ($c -gt 32767) { $c - 65536 } else { $c })?") }
    else { [void] $sb.Append($ch) }
  }
  return $sb.ToString()
}
$LicenseRtf = Join-Path $Stage 'license.rtf'
$rtf = '{\rtf1\ansi\ansicpg1252\deff0{\fonttbl{\f0\fswiss\fcharset0 Tahoma;}}\viewkind4\uc1\pard\sa120\f0\fs16 ' +
  ('\b Local Player RS\b0\par\par ' + (ConvertTo-RtfText 'Local Player RS is free software: a 100% local and offline media player in pure Rust. Licensed under the GNU General Public License, version 3 or later (GPL-3.0-or-later), with no warranty. The full license text is at https://www.gnu.org/licenses/. Source: https://github.com/Anon5T4R/Local-Player-RS.')) +
  "\par`r`n}`r`n"
[System.IO.File]::WriteAllText($LicenseRtf, $rtf, [System.Text.Encoding]::ASCII)

$Msi = Join-Path $Dist "local-player-rs-$Version-windows-$Arch.msi"
Invoke-Native 'wix build (per-machine)' {
  wix build (Join-Path $PSScriptRoot 'local-player-rs.wxs') -arch $Arch `
    -ext WixToolset.UI.wixext -ext WixToolset.Util.wixext `
    -d "Version=$MsiVersion" -d "BinDir=$Stage" -d "IconPath=$(Join-Path $Root 'icon.ico')" `
    -d "LicenseRtf=$LicenseRtf" `
    -o $Msi
}
# wix writes its debug symbols (.wixpdb) next to the MSI; keep them out of the release assets.
Remove-Item -Force -ErrorAction SilentlyContinue ([IO.Path]::ChangeExtension($Msi, '.wixpdb'))

# ---- portable zip ------------------------------------------------------------------------------
$Portable = Join-Path $TargetDir "windows-package\local-player-rs-$Version-windows-$Arch-portable"
Remove-Item -Recurse -Force $Portable -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force -Path $Portable | Out-Null
Copy-Item (Join-Path $Stage 'local-player-rs.exe') $Portable
foreach ($f in 'README.md') {
  $p = Join-Path $Root $f
  if (Test-Path $p) { Copy-Item $p $Portable }
}
$Zip = Join-Path $Dist "local-player-rs-$Version-windows-$Arch-portable.zip"
Remove-Item -Force $Zip -ErrorAction SilentlyContinue
# Compress-Archive (PowerShell 5.1) writes entry names with backslashes; the updater's portable
# unpack refuses them ("The portable ZIP contains an unsafe path or link."), and a zip with '\'
# is ambiguous cross-platform besides. Build the archive by hand, every entry with '/'.
Add-Type -AssemblyName System.IO.Compression, System.IO.Compression.FileSystem
$RootName = Split-Path $Portable -Leaf
$PortableFull = [System.IO.Path]::GetFullPath($Portable)
$ZipStream = [System.IO.File]::Open($Zip, [System.IO.FileMode]::Create)
$Archive = New-Object System.IO.Compression.ZipArchive($ZipStream, [System.IO.Compression.ZipArchiveMode]::Create)
try {
  Get-ChildItem -LiteralPath $Portable -Recurse -File | ForEach-Object {
    $Relative = [System.IO.Path]::GetFullPath($_.FullName).Substring($PortableFull.Length + 1) -replace '\\', '/'
    $Entry = $Archive.CreateEntry("$RootName/$Relative", [System.IO.Compression.CompressionLevel]::Optimal)
    $EntryStream = $Entry.Open()
    $FileStream = [System.IO.File]::OpenRead($_.FullName)
    try { $FileStream.CopyTo($EntryStream) } finally { $FileStream.Dispose(); $EntryStream.Dispose() }
  }
} finally {
  $Archive.Dispose()
  $ZipStream.Dispose()
}

Get-Item $Msi, $Zip | Format-Table Name, Length
