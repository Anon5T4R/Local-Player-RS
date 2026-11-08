# Medicao do --bench com contencao de CPU (avaliacao low-end do local-player-rs).
# ASCII-only: roda em PowerShell 5.1 (script sem BOM leria acento errado).
# Uso: powershell -ExecutionPolicy Bypass -File scripts\bench-contencao.ps1

$ErrorActionPreference = 'Stop'

$exe  = Join-Path $PSScriptRoot '..\target\release\local-player-rs.exe'
# Nome com caracteres japoneses: localiza pelo padrao em vez de cravar o texto.
$file = (Get-ChildItem 'C:\Dev\test-files\LocalMedia' -Filter 'Ado*.mkv' | Select-Object -First 1).FullName
if (-not (Test-Path $exe))  { throw "sem exe: $exe" }
if (-not $file -or -not (Test-Path $file)) { throw "sem arquivo de teste (Ado*.mkv em C:\Dev\test-files\LocalMedia)" }

$outDir = Join-Path $PSScriptRoot '..\target\bench'
New-Item -ItemType Directory -Force -Path $outDir | Out-Null
# Copia para um caminho ASCII: o nome original tem japones e -ArgumentList nao
# cita sozinho (o espaco quebrava o caminho).
$clip = Join-Path $outDir 'clip.mkv'
if (-not (Test-Path $clip)) { Copy-Item $file $clip }
$file = $clip

function Start-Burners([int]$n) {
    $procs = @()
    for ($i = 0; $i -lt $n; $i++) {
        $procs += Start-Process -WindowStyle Hidden -PassThru powershell `
            -ArgumentList '-NoProfile','-Command','$x=0; while ($true) { $x++ }'
    }
    return $procs
}

function Stop-Burners($procs) {
    foreach ($p in $procs) { try { $p.Kill() } catch { } }
}

function Run-Bench([string]$label, [int]$threads, [int]$reps) {
    $texts = @()
    for ($r = 0; $r -lt $reps; $r++) {
        $out = Join-Path $outDir ("{0}-{1}.txt" -f $label, $r)
        $env:LOCALPLAYER_DECODE_THREADS = "$threads"
        Start-Process -FilePath $exe -NoNewWindow -Wait -PassThru `
            -ArgumentList '--bench', $file, '--seconds', '30', '--hw', 'off' `
            -RedirectStandardOutput $out | Out-Null
        $line = Select-String -Path $out -Pattern '^decode ' | Select-Object -First 1
        if ($line) { $texts += $line.Line }
    }
    Write-Host "== $label (threads=$threads) =="
    foreach ($t in $texts) { Write-Host $t }
}

# Sem contencao: auto / 2 / 1 thread (3 repeticoes cada).
Run-Bench 'auto-livre' 0 3
Run-Bench 't2-livre'   2 3
Run-Bench 't1-livre'   1 3

# Contencao leve (3 loops de CPU, como pedido).
$burn3 = Start-Burners 3
try {
    Run-Bench 'auto-carga3' 0 3
    Run-Bench 't2-carga3'   2 3
} finally { Stop-Burners $burn3 }

# Contencao dura (8 loops: mais perto de um rustc usando todos os nucleos).
$burn8 = Start-Burners 8
try {
    Run-Bench 'auto-carga8' 0 2
    Run-Bench 't2-carga8'   2 2
} finally { Stop-Burners $burn8 }

$env:LOCALPLAYER_DECODE_THREADS = $null
Write-Host "saidas em $outDir"
