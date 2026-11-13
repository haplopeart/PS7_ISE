<#
.SYNOPSIS
    Builds PS7 ISE: the .NET engine that hosts PowerShell 7, then the Rust front-end.

.DESCRIPTION
    Everything lands in .\dist, which is the folder you copy or zip:

        dist\PS7_ISE.exe                      the front-end
        dist\PowerShellEngineHost.exe         the engine PS7_ISE.exe starts and talks to
        dist\System.Management.Automation.dll the PowerShell 7 engine
        dist\Modules\                         the PowerShell modules $PSHome expects

    The engine is published with a flat layout (RuntimeIdentifier=win-x64) because
    PowerShell derives $PSHome, and therefore $PSModulePath, from the folder that
    contains System.Management.Automation.dll. Without a Modules folder next to it the
    bundled cmdlets cannot load.

    Which PowerShell you get is decided by the Microsoft.PowerShell.SDK package version in
    PowerShellEngineHost\PowerShellEngineHost\PowerShellEngineHost.csproj. It needs the
    .NET runtime of the same generation on the machine (.NET 10 for PowerShell 7.6);
    -SelfContained puts that runtime in dist instead.
#>

[CmdletBinding()]
param(
    [string]$OutDir = 'dist',
    [switch]$SkipEngine,
    [switch]$SkipFrontEnd,
    # Bundle the .NET runtime into dist instead of requiring it on the machine.
    [switch]$SelfContained
)

$ErrorActionPreference = 'Stop'
$root = $PSScriptRoot
$dist = Join-Path $root $OutDir

# Reported in the build log; the csproj remains the one place that pins the version.
$csproj = Join-Path $root 'PowerShellEngineHost\PowerShellEngineHost\PowerShellEngineHost.csproj'
$PowerShellVersion = 'unknown'
if (Test-Path $csproj) {
    $match = Select-String -Path $csproj -Pattern '<PowerShellVersion>(.+)</PowerShellVersion>' |
        Select-Object -First 1
    if ($match) { $PowerShellVersion = $match.Matches[0].Groups[1].Value }
}

Write-Host "== PS7 ISE build -> $dist" -ForegroundColor Cyan

# cargo and dotnet write progress and warnings to standard error, which
# $ErrorActionPreference = 'Stop' would report as a failure of the script itself. Their
# exit code is the real signal, so the tools run with the preference relaxed.
function Invoke-Tool {
    param(
        [Parameter(Mandatory)][string]$Exe,
        [string[]]$Arguments = @()
    )
    $previous = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try { & $Exe @Arguments } finally { $ErrorActionPreference = $previous }
}

if (-not $SkipEngine) {
    Write-Host "-- Publishing PowerShellEngineHost (PowerShell $PowerShellVersion)" -ForegroundColor Yellow
    $project = Join-Path $root 'PowerShellEngineHost\PowerShellEngineHost\PowerShellEngineHost.csproj'

    # dotnet publish never removes anything, so a folder published with an older PowerShell
    # would keep that version's assemblies and modules side by side with the new ones.
    # dist also holds the front-end, which a -SkipFrontEnd run must survive.
    $frontEnd = Join-Path $dist 'PS7_ISE.exe'
    $stash = Join-Path ([IO.Path]::GetTempPath()) ('ps7ise-front-end-' + [Guid]::NewGuid().ToString('N'))
    if ($SkipFrontEnd -and (Test-Path $frontEnd)) { Move-Item $frontEnd $stash -Force }
    if (Test-Path $dist) {
        Remove-Item (Join-Path $dist '*') -Recurse -Force
    }

    try {
        if ($SelfContained) {
            # Carries the .NET runtime in dist, so the folder runs anywhere; costs ~200 MB.
            Invoke-Tool dotnet @('publish', $project, '-c', 'Release', '-o', $dist,
                '-r', 'win-x64', '--self-contained', 'true', '--nologo')
        } else {
            Invoke-Tool dotnet @('publish', $project, '-c', 'Release', '-o', $dist, '--nologo')
        }
        if ($LASTEXITCODE -ne 0) { throw "dotnet publish failed with exit code $LASTEXITCODE" }
    } finally {
        if (Test-Path $stash) {
            Move-Item $stash $frontEnd -Force
        }
    }

    # The SDK package keeps the modules under runtimes\<rid>\lib\<framework>\Modules, but a
    # RID-specific publish flattens only assemblies. PowerShell looks for them in
    # $PSHome\Modules - next to System.Management.Automation.dll - so copy them there.
    # The runtimes folder itself is left in place: depending on how the assembly is
    # resolved it may be used instead of the flat copy, and it holds the same Modules.
    # The framework directory is matched rather than named because it follows the
    # PowerShell release (net8.0 for 7.4, net9.0 for 7.5, net10.0 for 7.6).
    # runtimes\win and runtimes\unix both carry a Modules folder - the unix one is a subset
    # without the Windows-only modules - so the win copy has to win.
    $nested = Get-ChildItem (Join-Path $dist 'runtimes') -Directory -Recurse -Filter 'Modules' |
        Where-Object { $_.Parent.Name -like 'net*' } |
        Sort-Object { if ($_.FullName -match '\\runtimes\\win\\') { 0 } else { 1 } } |
        Select-Object -First 1
    if ($nested) {
        $target = Join-Path $dist 'Modules'
        if (-not (Test-Path $target)) { New-Item -ItemType Directory -Path $target | Out-Null }
        Copy-Item (Join-Path $nested.FullName '*') $target -Recurse -Force
        Write-Host "   modules: $($nested.FullName)" -ForegroundColor DarkGray
    } else {
        Write-Warning 'No Modules folder in the publish output - cmdlets may not load.'
    }

    # ref/ is compile-time only.
    $path = Join-Path $dist 'ref'
    if (Test-Path $path) { Remove-Item $path -Recurse -Force }
}

if (-not $SkipFrontEnd) {
    Write-Host '-- Building PS7_ISE.exe (Rust)' -ForegroundColor Yellow
    if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
        $cargo = Join-Path $env:USERPROFILE '.cargo\bin\cargo.exe'
        if (Test-Path $cargo) { $env:PATH = (Split-Path $cargo) + ';' + $env:PATH }
        else { throw 'cargo was not found - install Rust from https://rustup.rs' }
    }

    Push-Location $root
    try {
        Invoke-Tool cargo @('build', '--release')
        if ($LASTEXITCODE -ne 0) { throw "cargo build failed with exit code $LASTEXITCODE" }
    }
    finally {
        Pop-Location
    }

    Copy-Item (Join-Path $root 'target\release\PS7_ISE.exe') $dist -Force
}

$exe = Join-Path $dist 'PS7_ISE.exe'
$engine = Join-Path $dist 'PowerShellEngineHost.exe'
if (-not (Test-Path $exe)) { throw "Missing $exe" }
if (-not (Test-Path $engine)) { throw "Missing $engine - the engine was not published" }

Write-Host ''
Write-Host "Built: $exe" -ForegroundColor Green
Write-Host "Run it directly, or copy the whole $OutDir folder: PowerShell 7 runs in PowerShellEngineHost.exe, so the engine files must stay next to the exe."
