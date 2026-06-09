# TagDash — lancer en dev
# Usage : clic droit > "Exécuter avec PowerShell"  ou  .\dev.ps1 dans un terminal

$ErrorActionPreference = "Stop"

# Ajouter cargo au PATH si absent
if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    $env:Path = "$env:USERPROFILE\.cargo\bin;$env:Path"
}

# Se placer dans le dossier du projet (utile si lancé par double-clic)
$scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Definition
Set-Location $scriptDir

Write-Host "TagDash dev — $(Get-Date -Format 'HH:mm:ss')" -ForegroundColor Cyan
Write-Host "node  : $(node --version)"
Write-Host "cargo : $(cargo --version)"
Write-Host ""

npm run tauri:dev

# Pour tout builder et voir les erreurs de compilation Rust (si besoin) :
#cd "c:\Users\W11\SynologyDrive\Etienne Pro\Marchés\TagDash\src-tauri" && cargo clean && cargo build 2>&1 | tail -20