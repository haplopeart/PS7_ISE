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
    [switch]$SelfContained,
    # Rebuild without raising the patch number: for a repeat build of what is already there.
    [switch]$SkipVersionBump
)

$ErrorActionPreference = 'Stop'
$root = $PSScriptRoot
$dist = Join-Path $root $OutDir

# Version policy: the package version in Cargo.toml is the one number, and every rebuild of
# the exe raises its third digit. main.rs puts it in the title bar through CARGO_PKG_VERSION,
# build.rs writes it into the exe's version resource so Explorer's Properties sheet agrees,
# and the engine is published with the same three numbers. It is raised before anything is
# compiled so the engine and the front-end can never disagree.
function Set-BuildVersion {
    param(
        [Parameter(Mandatory)][string]$Manifest,
        [switch]$Bump
    )

    $text = Get-Content $Manifest -Raw
    # The first bare `version = "x.y.z"` in the file is the package's own: dependencies
    # write `name = { version = ... }`, which does not start the line.
    $found = [regex]::Match($text, '(?m)^version = "(\d+)\.(\d+)\.(\d+)"')
    if (-not $found.Success) { throw "No package version found in $Manifest" }

    $major = $found.Groups[1].Value
    $minor = $found.Groups[2].Value
    $patch = $found.Groups[3].Value
    if ($Bump) { $patch = ([int]$patch + 1).ToString() }
    $version = "$major.$minor.$patch"

    if ($Bump) {
        $line = "version = `"$version`""
        $text = $text.Remove($found.Index, $found.Length).Insert($found.Index, $line)
        # Without a byte order mark: cargo reads the manifest as plain UTF-8.
        [IO.File]::WriteAllText($Manifest, $text, (New-Object System.Text.UTF8Encoding($false)))
    }
    return $version
}

$Version = Set-BuildVersion -Manifest (Join-Path $root 'Cargo.toml') -Bump:(-not $SkipVersionBump)
# Each property in its own parentheses: the comma binds tighter than +, so a bare
# `'-p:Version=' + $Version, ...` hands dotnet one long argument instead of three.
$assemblyInfo = @(
    ('-p:Version=' + $Version),
    ('-p:AssemblyVersion=' + $Version),
    ('-p:FileVersion=' + $Version)
)

# Reported in the build log; the csproj remains the one place that pins the version.
$csproj = Join-Path $root 'PowerShellEngineHost\PowerShellEngineHost\PowerShellEngineHost.csproj'
$PowerShellVersion = 'unknown'
if (Test-Path $csproj) {
    $match = Select-String -Path $csproj -Pattern '<PowerShellVersion>(.+)</PowerShellVersion>' |
        Select-Object -First 1
    if ($match) { $PowerShellVersion = $match.Matches[0].Groups[1].Value }
}

Write-Host "== PS7 ISE $Version build -> $dist" -ForegroundColor Cyan

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
    # dist also holds the front-end and the job host, which must survive.
    $keep = @('PS7_ISE.exe', 'pwsh.exe', 'pwsh.dll', 'pwsh.deps.json', 'pwsh.runtimeconfig.json')
    $stashDir = Join-Path ([IO.Path]::GetTempPath()) ('ps7ise-stash-' + [Guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $stashDir -Force | Out-Null
    
    foreach ($name in $keep) {
        $path = Join-Path $dist $name
        if (Test-Path $path) { Move-Item $path $stashDir -Force }
    }

    if (Test-Path $dist) {
        Remove-Item (Join-Path $dist '*') -Recurse -Force
    }

    try {
        if ($SelfContained) {
            # Carries the .NET runtime in dist, so the folder runs anywhere; costs ~200 MB.
            Invoke-Tool dotnet (@('publish', $project, '-c', 'Release', '-o', $dist,
                '-r', 'win-x64', '--self-contained', 'true', '--nologo') + $assemblyInfo)
        } else {
            Invoke-Tool dotnet (@('publish', $project, '-c', 'Release', '-o', $dist,
                '--nologo') + $assemblyInfo)
        }
        if ($LASTEXITCODE -ne 0) { throw "dotnet publish failed with exit code $LASTEXITCODE" }
    } finally {
        foreach ($file in Get-ChildItem $stashDir -File) {
            Move-Item $file.FullName (Join-Path $dist $file.Name) -Force
        }
        Remove-Item $stashDir -Recurse -Force
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

# The PowerShell 7 install ships hostfxr.dll and hostpolicy.dll, and both the SDK's flat
# publish and the job host's copy of PowerShell bring them into dist. They are the host's own
# runtime loader: sitting in the application folder they make every .NET program there look
# for Microsoft.NETCore.App in that folder instead of in the machine-wide shared directory,
# and a framework-dependent app then dies with "No frameworks were found". They have to go
# whatever the build did, including a front-end-only run that did not republish the engine.
foreach ($dll in @('hostfxr.dll', 'hostpolicy.dll')) {
    $path = Join-Path $dist $dll
    if (Test-Path $path) {
        try { Remove-Item $path -Force -ErrorAction Stop }
        catch { Write-Host ("  (could not remove " + $dll + ": it is in use)") -ForegroundColor DarkYellow }
    }
}

$exe = Join-Path $dist 'PS7_ISE.exe'
$engine = Join-Path $dist 'PowerShellEngineHost.exe'
if (-not (Test-Path $exe)) { throw "Missing $exe" }
if (-not (Test-Path $engine)) { throw "Missing $engine - the engine was not published" }

Write-Host ''
Write-Host "Built: $exe" -ForegroundColor Green
Write-Host "Run it directly, or copy the whole $OutDir folder: PowerShell 7 runs in PowerShellEngineHost.exe, so the engine files must stay next to the exe."
