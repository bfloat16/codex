<#
.SYNOPSIS
Configures Cargo to use the Codex-built rusty_v8 artifacts on Windows.

.DESCRIPTION
Downloads and verifies the same rusty_v8 archive and generated bindings used
by .github/actions/setup-rusty-v8, then exports their paths for the current
PowerShell process.

.EXAMPLE
. .\env-windows.ps1
cargo check --workspace
#>

[CmdletBinding()]
param(
    [string] $Target,
    [string] $CacheDirectory
)

& {
    param(
        [string] $RequestedTarget,
        [string] $RequestedCacheDirectory
    )

    $ErrorActionPreference = 'Stop'
    $ProgressPreference = 'SilentlyContinue'

    function Invoke-AssetDownload {
        param(
            [Parameter(Mandatory)]
            [string] $Uri,
            [Parameter(Mandatory)]
            [string] $OutFile
        )

        $curlCommand = Get-Command curl.exe -ErrorAction SilentlyContinue
        if ($null -ne $curlCommand) {
            & $curlCommand.Source `
                --fail `
                --location `
                --silent `
                --show-error `
                --retry 3 `
                --retry-all-errors `
                --connect-timeout 30 `
                --output $OutFile `
                $Uri
            if ($LASTEXITCODE -ne 0) {
                throw "Failed to download $Uri with curl."
            }
            return
        }

        for ($attempt = 1; $attempt -le 3; $attempt++) {
            try {
                Invoke-WebRequest -UseBasicParsing -Uri $Uri -OutFile $OutFile
                return
            }
            catch {
                Remove-Item -LiteralPath $OutFile -Force -ErrorAction SilentlyContinue
                if ($attempt -eq 3) {
                    throw
                }
                Start-Sleep -Seconds (2 * $attempt)
            }
        }
    }

    $cargoTomlPath = Join-Path $PSScriptRoot '..\Cargo.toml'
    $versionMatches = @(
        Select-String -LiteralPath $cargoTomlPath -Pattern '^v8\s*=\s*"=(\d+\.\d+\.\d+)"\s*$'
    )
    if ($versionMatches.Count -ne 1) {
        throw "Expected exactly one pinned v8 version in $cargoTomlPath"
    }
    $version = $versionMatches[0].Matches[0].Groups[1].Value

    if ([string]::IsNullOrWhiteSpace($RequestedTarget)) {
        $rustcVersion = & rustc -vV
        if ($LASTEXITCODE -ne 0) {
            throw 'Failed to determine the host target with rustc.'
        }
        $hostLine = $rustcVersion | Where-Object { $_ -match '^host:\s+(.+)$' }
        if ($null -eq $hostLine -or $hostLine.Count -ne 1) {
            throw 'Unable to determine the host target from rustc -vV.'
        }
        $RequestedTarget = [regex]::Match($hostLine, '^host:\s+(.+)$').Groups[1].Value
    }

    $supportedTargets = @(
        'x86_64-pc-windows-msvc',
        'aarch64-pc-windows-msvc'
    )
    if ($RequestedTarget -notin $supportedTargets) {
        throw "Unsupported Windows target: $RequestedTarget"
    }

    if ([string]::IsNullOrWhiteSpace($RequestedCacheDirectory)) {
        $localAppData = [Environment]::GetFolderPath('LocalApplicationData')
        if ([string]::IsNullOrWhiteSpace($localAppData)) {
            throw 'Unable to determine the LocalApplicationData directory.'
        }
        $RequestedCacheDirectory = Join-Path $localAppData 'codex\rusty-v8'
    }

    $profile = 'ptrcomp_sandbox_release'
    $releaseTag = "rusty-v8-v$version"
    $baseUrl = "https://github.com/openai/codex/releases/download/$releaseTag"
    $artifactDirectory = Join-Path $RequestedCacheDirectory "$version\$RequestedTarget"
    $archiveName = "rusty_v8_${profile}_${RequestedTarget}.lib.gz"
    $bindingName = "src_binding_${profile}_${RequestedTarget}.rs"
    $checksumsName = "rusty_v8_${profile}_${RequestedTarget}.sha256"
    $checksumsPath = Join-Path $artifactDirectory $checksumsName

    New-Item -ItemType Directory -Path $artifactDirectory -Force | Out-Null

    $checksumsTemporaryPath = "$checksumsPath.$PID.tmp"
    try {
        Invoke-AssetDownload -Uri "$baseUrl/$checksumsName" -OutFile $checksumsTemporaryPath
        Move-Item -LiteralPath $checksumsTemporaryPath -Destination $checksumsPath -Force
    }
    finally {
        Remove-Item -LiteralPath $checksumsTemporaryPath -Force -ErrorAction SilentlyContinue
    }

    $checksumLines = @(
        Get-Content -LiteralPath $checksumsPath |
            Where-Object { -not [string]::IsNullOrWhiteSpace($_) }
    )
    if ($checksumLines.Count -ne 2) {
        throw "Expected exactly two checksums in $checksumsPath"
    }

    $checksums = @{}
    foreach ($line in $checksumLines) {
        if ($line -notmatch '^([0-9a-fA-F]{64})\s+\*?([^\\/]+)\s*$') {
            throw "Invalid checksum entry in ${checksumsPath}: $line"
        }
        $checksums[$Matches[2]] = $Matches[1].ToLowerInvariant()
    }

    foreach ($assetName in @($archiveName, $bindingName)) {
        if (-not $checksums.ContainsKey($assetName)) {
            throw "Missing checksum for $assetName in $checksumsPath"
        }

        $assetPath = Join-Path $artifactDirectory $assetName
        $expectedHash = $checksums[$assetName]
        $hasValidCachedAsset = $false
        if (Test-Path -LiteralPath $assetPath -PathType Leaf) {
            $cachedHash = (Get-FileHash -LiteralPath $assetPath -Algorithm SHA256).Hash.ToLowerInvariant()
            $hasValidCachedAsset = $cachedHash -eq $expectedHash
        }

        if (-not $hasValidCachedAsset) {
            $temporaryPath = "$assetPath.$PID.tmp"
            try {
                Write-Host "Downloading $assetName"
                Invoke-AssetDownload -Uri "$baseUrl/$assetName" -OutFile $temporaryPath
                $actualHash = (Get-FileHash -LiteralPath $temporaryPath -Algorithm SHA256).Hash.ToLowerInvariant()
                if ($actualHash -ne $expectedHash) {
                    throw "Checksum mismatch for $assetName (expected $expectedHash, got $actualHash)"
                }
                Move-Item -LiteralPath $temporaryPath -Destination $assetPath -Force
            }
            finally {
                Remove-Item -LiteralPath $temporaryPath -Force -ErrorAction SilentlyContinue
            }
        }
    }

    $env:RUSTY_V8_ARCHIVE = Join-Path $artifactDirectory $archiveName
    $env:RUSTY_V8_SRC_BINDING_PATH = Join-Path $artifactDirectory $bindingName

    Write-Host "RUSTY_V8_ARCHIVE=$env:RUSTY_V8_ARCHIVE"
    Write-Host "RUSTY_V8_SRC_BINDING_PATH=$env:RUSTY_V8_SRC_BINDING_PATH"
} $Target $CacheDirectory
