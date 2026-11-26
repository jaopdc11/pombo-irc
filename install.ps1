# Builds the pombo client from source and installs it as `pombo` on Windows.
#
#   powershell -ExecutionPolicy Bypass -File .\install.ps1              install for this user
#   powershell -ExecutionPolicy Bypass -File .\install.ps1 -Uninstall   remove it
#
# Installs to %LOCALAPPDATA%\Programs\pombo and adds that folder to the user PATH.
param([switch]$Uninstall)

$ErrorActionPreference = 'Stop'

$BinDir = Join-Path $env:LOCALAPPDATA 'Programs\pombo'
$Target = Join-Path $BinDir 'pombo.exe'
$RepoDir = $PSScriptRoot

function Say($msg) { Write-Host '==> ' -ForegroundColor Cyan -NoNewline; Write-Host $msg }
function Die($msg) { Write-Host 'error: ' -ForegroundColor Red -NoNewline; Write-Host $msg; exit 1 }

function Get-UserPath { [Environment]::GetEnvironmentVariable('Path', 'User') }
function Set-UserPath($value) { [Environment]::SetEnvironmentVariable('Path', $value, 'User') }
function Split-PathList($value) { @($value -split ';' | Where-Object { $_ -ne '' }) }

if ($Uninstall) {
    if (Test-Path $Target) {
        Remove-Item -Force $Target
        if (-not (Get-ChildItem $BinDir -Force | Select-Object -First 1)) { Remove-Item -Force $BinDir }
        Say "removed $Target"
    } else {
        Say "nothing to remove at $Target"
    }
    $kept = Split-PathList (Get-UserPath) | Where-Object { $_ -ne $BinDir }
    Set-UserPath ($kept -join ';')
    exit 0
}

if (-not $IsWindows -and $env:OS -ne 'Windows_NT') { Die 'install.ps1 is for Windows; on Linux use ./install.sh' }

if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    $cargoBin = Join-Path $env:USERPROFILE '.cargo\bin'
    if (Test-Path (Join-Path $cargoBin 'cargo.exe')) { $env:Path = "$cargoBin;$env:Path" }
}
if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    Die @'
Rust is not installed. Get it from https://rustup.rs (rustup-init.exe); let it install the
Visual Studio Build Tools when it asks, since they provide the linker and the C compiler the
TLS library needs. Then open a new terminal and run install.ps1 again.
'@
}

# The relay only accepts clients built from its own commit, which the build reads from git.
if (-not (Get-Command git -ErrorAction SilentlyContinue)) {
    Die @'
git is not installed. pombo stamps the commit it was built from into the binary and the relay
refuses clients without it. Install git from https://git-scm.com, clone the repository with it,
then open a new terminal and run install.ps1 again.
'@
}
git -C $RepoDir rev-parse HEAD *> $null
if ($LASTEXITCODE -ne 0) { Die "$RepoDir is not a git clone; clone the repository with git and run install.ps1 from it" }

Say 'building pombo (first build takes a minute or two)'
Push-Location $RepoDir
try {
    cargo build --release --locked -p pombo-irc --bin pombo
    if ($LASTEXITCODE -ne 0) { Die "build failed. If your Rust is old, run 'rustup update' and try again." }
} finally {
    Pop-Location
}

New-Item -ItemType Directory -Force -Path $BinDir | Out-Null
Copy-Item -Force (Join-Path $RepoDir 'target\release\pombo.exe') $Target
Say "installed $Target"

$userPath = Split-PathList (Get-UserPath)
if ($userPath -notcontains $BinDir) {
    Set-UserPath ((@($userPath) + $BinDir) -join ';')
    Say "added $BinDir to your PATH. Open a new terminal and run: pombo"
} else {
    Say 'done. Run: pombo'
}
