# Build download-core with MSVC env (VS DevShell).
# Usage: ./scripts/build-core.ps1 [--release] [-- <extra cargo args>]

$ErrorActionPreference = "Stop"

$vswhere = Join-Path ${env:ProgramFiles(x86)} "Microsoft Visual Studio\Installer\vswhere.exe"
if (-not (Test-Path $vswhere)) {
    Write-Error "vswhere.exe not found. Install Visual Studio 2022 Build Tools with VC Tools."
}

$vs = & $vswhere -latest -products * `
    -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 `
    -property installationPath
if (-not $vs) {
    Write-Error "No VS install with Microsoft.VisualStudio.Component.VC.Tools.x86.x64 found."
}

$devShell = Join-Path $vs "Common7\Tools\Microsoft.VisualStudio.DevShell.dll"
Import-Module $devShell
Enter-VsDevShell -VsInstallPath $vs -SkipAutomaticLocation -DevCmdArguments '-arch=x64' | Out-Null

$repoRoot = Split-Path -Parent $PSScriptRoot
Set-Location $repoRoot

$cargoArgs = @("build", "-p", "download-core")
if ($args -contains "--release") {
    $cargoArgs += "--release"
    $args = $args | Where-Object { $_ -ne "--release" }
}
if ($args.Count -gt 0) {
    $cargoArgs += $args
}

Write-Host "VCINSTALLDIR=$env:VCINSTALLDIR"
Write-Host "cargo $($cargoArgs -join ' ')"
& cargo @cargoArgs
exit $LASTEXITCODE
