# Fetches the model weights into .\models\ (gitignored).
#
# The shipping app can download these itself, into %APPDATA%\Lathe\models, from the
# Models section of the settings window. This script is the equivalent for a checkout:
# it gets a working set in one command, and it is what the tools in crates/spike run
# against.
#
# The files here are the ones config.rs actually defaults to. They are kept in step with
# crates/core/src/download.rs -- if you change a default there, change it here too, or a
# fresh clone will download models the app then cannot find.
#
# Roughly 7.4 GB with -Multilingual, 5.0 GB without.

param(
    # Also fetch Gemma 3, which cleans up dictation in languages S1-mini does not cover
    # (amendment A21). Skip it if you only ever dictate in English -- non-English speech
    # is still recognised without it, just pasted uncleaned.
    [switch]$Multilingual
)

$ErrorActionPreference = 'Stop'

$root = Split-Path -Parent $PSScriptRoot
$dest = Join-Path $root 'models'
New-Item -ItemType Directory -Force -Path $dest | Out-Null

$files = @(
    # Speech. Q8_0 rather than Q5_0 or F16: measured on an RX 6600 XT, Q8_0 matches
    # Q5_0's speed at higher precision while F16 is 18% slower for no gain. Amendment A23.
    @{ repo = 'cstr/cohere-transcribe-03-2026-GGUF'; file = 'cohere-transcribe-q8_0.gguf'; note = 'Cohere Transcribe Q8_0, ~2.3 GB' }
    # Silence gate, brief 6.1. v6.2.0 exists upstream; v5.1.2 is the revision whisper.cpp
    # documents against.
    @{ repo = 'ggml-org/whisper-vad'; file = 'ggml-silero-v5.1.2.bin'; note = 'Silero VAD, ~2 MB' }
    # English cleanup, brief 4.2. Q8_0, our own conversion of Superwhisper's F16: identical
    # output on 94-95% of dictations, no content loss, half the size. Q4_K_M was observed
    # dropping a clause outright. Amendments A23 and A27.
    @{ repo = 'stanthegorilla/S1-mini-Q8_0-GGUF'; file = 's1-mini-q8_0.gguf'; note = 'S1-mini Q8_0, ~0.8 GB' }
    # Brief 4.2 requires shipping these alongside the model.
    @{ repo = 'superwhisper/s1-mini-GGUF'; file = 'LICENSE'; note = 'S1-mini licence' }
    @{ repo = 'superwhisper/s1-mini-GGUF'; file = 'NOTICE'; note = 'S1-mini notice' }
)

if ($Multilingual) {
    # Gemma 4 E2B, Google's own QAT GGUF (this repository is not gated, unlike Gemma 3's).
    # Measured on 100 Polish dictations against Gemma 3 4B: lower error rate, a quarter
    # of the content losses, same speed, and 40% less graphics memory because its
    # per-layer embeddings stay in system RAM. Amendment A28.
    $files += @{ repo = 'google/gemma-4-E2B-it-qat-q4_0-gguf'; file = 'gemma-4-E2B_q4_0-it.gguf'; note = 'Gemma 4 E2B QAT Q4_0, ~3.3 GB' }
}

foreach ($f in $files) {
    $out = Join-Path $dest $f.file
    if (Test-Path $out) {
        Write-Host "have    $($f.file)"
        continue
    }
    $url = "https://huggingface.co/$($f.repo)/resolve/main/$($f.file)"
    Write-Host "fetch   $($f.file)  --  $($f.note)"
    # Downloaded to .part and moved on success, so an interrupted run cannot leave a
    # truncated file that later looks present.
    $tmp = "$out.part"
    Invoke-WebRequest -Uri $url -OutFile $tmp -UseBasicParsing
    Move-Item -Force $tmp $out
}

Write-Host ''
Get-ChildItem $dest | Select-Object Name, @{n='MB';e={[math]::Round($_.Length / 1MB, 1)}} | Format-Table -AutoSize

if (-not $Multilingual) {
    Write-Host 'English only. Re-run with -Multilingual to add Gemma 3 for other languages.'
}
