# Zeron v3 for Windows: the zgx interface (apps/zgx, its `beta` feature: the
# production edge and Zeron's own engine and data) with the engine built from
# the same tree, packaged as a portable zip and a per-user installer.
#
#   zeron.exe          apps/zgx's zeron-zgx, built with --features beta
#   zeron-engine.exe   the root workspace's engine (crates/launch finds it
#                      beside the app); an engine already serving port 27654,
#                      such as the v2 app's, is used as it is
#   ui\, plugins-builtin\   the interface's scripts, read where they lie
#   zeron.ico, LICENSE, THIRD_PARTY_NOTICES.md, licenses\
#
# No zeron-update.json: the Windows updater swaps zeron.exe alone, which
# would leave ui\ and the engine behind. v3 installs update by hand.
#
#   ./scripts/package-v3-windows.ps1 -Src ../zeron-nightly-src [-CargoArgs '-j','4']
#
# Writes <Src>/target/package-v3/zeron-<version>-windows-<arch>(.zip|-setup.exe).
param(
    [Parameter(Mandatory)][string]$Src,
    [string[]]$CargoArgs = @(),
    [switch]$NoInstaller
)
$ErrorActionPreference = 'Stop'
$tools = Split-Path $PSScriptRoot -Parent
$root = (Resolve-Path -LiteralPath $Src).Path

function Find-InnoSetupCompiler {
    $command = Get-Command 'ISCC.exe' -ErrorAction SilentlyContinue
    if ($command) { return $command.Source }
    $bases = @(${env:ProgramFiles(x86)}, $env:ProgramFiles, "$env:LOCALAPPDATA/Programs") | Where-Object { $_ }
    foreach ($base in $bases) {
        $candidate = Join-Path $base 'Inno Setup 6/ISCC.exe'
        if (Test-Path -LiteralPath $candidate) { return $candidate }
    }
    throw 'Inno Setup 6 (ISCC.exe) is required to build the installer: winget install JRSoftware.InnoSetup'
}

function Get-WindowsPackageArch([string]$Path) {
    $stream = [IO.File]::OpenRead($Path)
    try {
        $reader = [IO.BinaryReader]::new($stream)
        $stream.Position = 0x3C
        $stream.Position = $reader.ReadUInt32()
        if ($reader.ReadUInt32() -ne 0x4550) { throw "Not a PE executable: $Path" }
        $machine = $reader.ReadUInt16()
    } finally { $stream.Dispose() }
    switch ($machine) {
        0x8664 { 'x86_64' }
        0xAA64 { 'aarch64' }
        default { throw ('Unsupported Windows executable machine type: 0x{0:X4}' -f $machine) }
    }
}

# `--version` of a GUI-subsystem executable, read through explicit pipes.
function Get-ExeOutput([string]$Path, [string]$Arguments) {
    $probe = [Diagnostics.ProcessStartInfo]::new()
    $probe.FileName = $Path
    $probe.Arguments = $Arguments
    $probe.UseShellExecute = $false
    $probe.CreateNoWindow = $true
    $probe.RedirectStandardOutput = $true
    $probe.RedirectStandardError = $true
    $process = [Diagnostics.Process]::Start($probe)
    try {
        $stdout = $process.StandardOutput.ReadToEndAsync()
        $stderr = $process.StandardError.ReadToEndAsync()
        if (-not $process.WaitForExit(10000)) {
            $process.Kill()
            throw "$Path $Arguments timed out"
        }
        if ($process.ExitCode -ne 0) { throw "$Path $Arguments failed: $($stderr.Result)" }
        return $stdout.Result.Trim()
    } finally { $process.Dispose() }
}

function Copy-Tree([string]$From, [string]$To, [string[]]$ExcludeDirs) {
    robocopy $From $To /E /NFL /NDL /NJH /NJS /NP /XD @ExcludeDirs /XF .lock .DS_Store | Out-Null
    if ($LASTEXITCODE -ge 8) { throw "Copying $From failed (robocopy $LASTEXITCODE)" }
    $global:LASTEXITCODE = 0
}

Push-Location $root
try {
    $manifest = Get-Content -Raw -LiteralPath 'Cargo.toml'
    $found = [regex]::Match($manifest, '(?ms)^\[workspace\.package\]\r?$.*?^version = "([^"]+)"')
    if (-not $found.Success) { throw 'No [workspace.package] version in Cargo.toml' }
    $version = $found.Groups[1].Value
    $numericVersion = ($version -split '-', 2)[0]

    cargo build --release --locked -p zeron --bin zeron-engine @CargoArgs
    if ($LASTEXITCODE -ne 0) { throw 'Engine build failed' }
    $engine = (Resolve-Path -LiteralPath './target/release/zeron-engine.exe').Path

    # apps/zgx is a workspace of its own; cargo reads its .cargo/config.toml
    # by where it runs, so build from its folder.
    Push-Location 'apps/zgx'
    try {
        $env:ZERON_BETA_VERSION = $version
        cargo build --release --locked --features beta @CargoArgs
        if ($LASTEXITCODE -ne 0) { throw 'Interface build failed' }
    } finally {
        Remove-Item Env:ZERON_BETA_VERSION -ErrorAction SilentlyContinue
        Pop-Location
    }
    $app = (Resolve-Path -LiteralPath './apps/zgx/target/release/zeron-zgx.exe').Path

    $said = Get-ExeOutput $app '--version'
    if ($said -ne "zeron $version") { throw "zeron-zgx.exe says '$said', not 'zeron $version'" }
    # Production's only: neither binary may carry staging's edge or sign-in.
    foreach ($bin in $app, $engine) {
        $text = [Text.Encoding]::ASCII.GetString([IO.File]::ReadAllBytes($bin))
        if ($text -match 'staging\.zeron\.sh|zeron-edge-staging|client_01KWD0E9SYPV2WE4QN9TP9DP0W') {
            throw "$bin carries staging's edge or sign-in client"
        }
    }

    $arch = Get-WindowsPackageArch $app
    $out = Join-Path $root 'target/package-v3'
    $stage = Join-Path $out "zeron-$version-windows-$arch"
    if (Test-Path -LiteralPath $stage) { Remove-Item -LiteralPath $stage -Recurse -Force }
    New-Item -ItemType Directory -Force -Path $stage | Out-Null
    Copy-Item -LiteralPath $app -Destination (Join-Path $stage 'zeron.exe')
    Copy-Item -LiteralPath $engine -Destination (Join-Path $stage 'zeron-engine.exe')
    Copy-Tree (Join-Path $root 'apps/zgx/ui') (Join-Path $stage 'ui') @('.zgx', (Join-Path $root 'apps/zgx/ui/tests'))
    Copy-Tree (Join-Path $root 'apps/zgx/plugins-builtin') (Join-Path $stage 'plugins-builtin') @('.zgx')
    if (-not (Test-Path -LiteralPath (Join-Path $stage 'ui/app.luau'))) { throw 'ui/app.luau missing from the package' }
    Copy-Item -LiteralPath 'dist/windows/zeron.ico', 'LICENSE', 'THIRD_PARTY_NOTICES.md' -Destination $stage
    $licenses = Join-Path $stage 'licenses/fonts'
    New-Item -ItemType Directory -Force -Path $licenses | Out-Null
    Copy-Item -Path 'crates/ui/assets/fonts/licenses/*', 'apps/zgx/fonts/*-OFL.txt' -Destination $licenses
    Copy-Item -LiteralPath 'crates/voice/NOTICE.md' -Destination (Join-Path $stage 'licenses/parakeet-v3.txt')

    $zip = "$stage.zip"
    Compress-Archive -Path "$stage/*" -DestinationPath $zip -Force
    Write-Host "Packaged $zip"
    if (-not $NoInstaller) {
        $iscc = Find-InnoSetupCompiler
        & $iscc /Qp "/DAppVersion=$version" "/DNumericVersion=$numericVersion" "/DArch=$arch" `
            "/DPackageDir=$([IO.Path]::GetFullPath($stage))" `
            "/DOutputDir=$([IO.Path]::GetFullPath($out))" `
            "/DIconFile=$([IO.Path]::GetFullPath((Join-Path $root 'dist/windows/zeron.ico')))" `
            ([IO.Path]::GetFullPath((Join-Path $tools 'dist/zeron-v3.iss')))
        if ($LASTEXITCODE -ne 0) { throw 'Installer build failed' }
        $setup = Join-Path $out "zeron-$version-windows-$arch-setup.exe"
        if (-not (Test-Path -LiteralPath $setup)) { throw "Installer not produced: $setup" }
        Write-Host "Packaged $setup"
    }
} finally { Pop-Location }
