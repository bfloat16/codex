<#
.SYNOPSIS
Builds the Windows binaries shipped by the Codex release workflow.

.DESCRIPTION
Uses the same binary allowlist, rusty_v8 artifacts, MSVC environment, and
x86_64 SQLite setting as .github/workflows/rust-release-windows.yml. The
experimental voice host is intentionally outside the release bundles and is
therefore not built.

.EXAMPLE
.\scripts\build-windows.ps1

.EXAMPLE
.\scripts\build-windows.ps1 -Bundle primary

.EXAMPLE
.\scripts\build-windows.ps1 -Target aarch64-pc-windows-msvc
#>

[CmdletBinding()]
param(
    [ValidateSet(
        'x86_64-pc-windows-msvc',
        'aarch64-pc-windows-msvc'
    )]
    [string] $Target,

    [ValidateSet('all', 'primary', 'helpers', 'app-server')]
    [string] $Bundle = 'all',

    [string] $V8CacheDirectory
)

$ErrorActionPreference = 'Stop'

function Import-MsvcEnvironment {
    param(
        [Parameter(Mandatory)]
        [string] $RequestedTarget,

        [Parameter(Mandatory)]
        [string] $RepositoryRoot
    )

    $setupScript = Join-Path $RepositoryRoot '.github\actions\setup-msvc-env\setup-msvc-env.ps1'
    $temporaryRoot = [IO.Path]::GetTempPath()
    $githubEnvironmentPath = Join-Path $temporaryRoot "codex-msvc-env-$PID-$([guid]::NewGuid()).txt"
    $previousGithubEnvironment = $env:GITHUB_ENV
    $previousRunnerTemp = $env:RUNNER_TEMP

    try {
        $env:GITHUB_ENV = $githubEnvironmentPath
        if ([string]::IsNullOrWhiteSpace($env:RUNNER_TEMP)) {
            $env:RUNNER_TEMP = $temporaryRoot
        }

        & $setupScript -Target $RequestedTarget
        if ($LASTEXITCODE -ne 0) {
            throw "Failed to configure the MSVC environment for $RequestedTarget."
        }

        foreach ($line in Get-Content -LiteralPath $githubEnvironmentPath) {
            if ($line -notmatch '^([^=]+)=(.*)$') {
                throw "Invalid environment entry emitted by ${setupScript}: $line"
            }
            Set-Item -Path "Env:$($Matches[1])" -Value $Matches[2]
        }
    }
    finally {
        if ($null -eq $previousGithubEnvironment) {
            Remove-Item Env:GITHUB_ENV -ErrorAction SilentlyContinue
        }
        else {
            $env:GITHUB_ENV = $previousGithubEnvironment
        }

        if ($null -eq $previousRunnerTemp) {
            Remove-Item Env:RUNNER_TEMP -ErrorAction SilentlyContinue
        }
        else {
            $env:RUNNER_TEMP = $previousRunnerTemp
        }

        Remove-Item -LiteralPath $githubEnvironmentPath -Force -ErrorAction SilentlyContinue
    }
}

$codexRsRoot = Split-Path -Parent $PSScriptRoot
$repositoryRoot = Split-Path -Parent $codexRsRoot
$usesExplicitTarget = -not [string]::IsNullOrWhiteSpace($Target)

if (-not $usesExplicitTarget) {
    $rustcVersion = & rustc -vV
    if ($LASTEXITCODE -ne 0) {
        throw 'Failed to determine the host target with rustc.'
    }

    $hostLines = @($rustcVersion | Where-Object { $_ -match '^host:\s+(.+)$' })
    if ($hostLines.Count -ne 1) {
        throw 'Unable to determine the host target from rustc -vV.'
    }

    $Target = [regex]::Match($hostLines[0], '^host:\s+(.+)$').Groups[1].Value
}

$supportedTargets = @(
    'x86_64-pc-windows-msvc',
    'aarch64-pc-windows-msvc'
)
if ($Target -notin $supportedTargets) {
    throw "Unsupported Windows target: $Target"
}

$releaseBundles = [ordered]@{
    primary      = @(
        'codex',
        'codex-code-mode-host'
    )
    helpers      = @(
        'codex-windows-sandbox-setup',
        'codex-command-runner'
    )
    'app-server' = @(
        'codex-code-mode-host'
    )
}

$binaries = if ($Bundle -eq 'all') {
    @($releaseBundles.Values | ForEach-Object { $_ } | Select-Object -Unique)
}
else {
    @($releaseBundles[$Bundle])
}

Import-MsvcEnvironment -RequestedTarget $Target -RepositoryRoot $repositoryRoot

$v8EnvironmentScript = Join-Path $PSScriptRoot 'env-windows.ps1'
. $v8EnvironmentScript -Target $Target -CacheDirectory $V8CacheDirectory

if ($Target -eq 'x86_64-pc-windows-msvc') {
    $env:LIBSQLITE3_FLAGS = 'SQLITE_DISABLE_INTRINSIC'
}

$cargoArguments = @(
    'build',
    '--release'
)
if ($usesExplicitTarget) {
    $cargoArguments += @('--target', $Target)
}
foreach ($binary in $binaries) {
    $cargoArguments += @('--bin', $binary)
}

Write-Host "Building Windows $Bundle bundle for $Target"
Write-Host "Binaries: $($binaries -join ', ')"

Push-Location $codexRsRoot
try {
    & cargo @cargoArguments
    if ($LASTEXITCODE -ne 0) {
        throw "Cargo build failed with exit code $LASTEXITCODE."
    }
}
finally {
    Pop-Location
}

$cargoTargetRoot = if ([string]::IsNullOrWhiteSpace($env:CARGO_TARGET_DIR)) {
    Join-Path $codexRsRoot 'target'
}
elseif ([IO.Path]::IsPathRooted($env:CARGO_TARGET_DIR)) {
    $env:CARGO_TARGET_DIR
}
else {
    Join-Path $codexRsRoot $env:CARGO_TARGET_DIR
}
$releaseDirectory = if ($usesExplicitTarget) {
    Join-Path $cargoTargetRoot "$Target\release"
}
else {
    Join-Path $cargoTargetRoot 'release'
}
Write-Host "Build complete: $releaseDirectory"
