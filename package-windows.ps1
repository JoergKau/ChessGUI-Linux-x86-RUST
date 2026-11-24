#!/usr/bin/env pwsh
# package-windows.ps1 - build SiliconBoard and assemble the Windows release.
#
# The steps are the manual ones from the README, scripted:
#
#   1. `cargo build --release`
#   2. assemble `SiliconBoard-<version>-windows-x86_64\`: the executable in
#      `bin\`, the asset tree in `public\` (where the resolver looks when
#      walking up from the executable - unit-tested in src/assets.rs), the
#      start-up PGN in `PGN\`, LICENSE (GPL), LICENSE-MIT,
#      THIRD-PARTY-NOTICES.md, README.md, RELEASE_NOTES.md and
#      features-list.md,
#   3. build `SiliconBoard-<version>-windows-x86_64.zip` and print its
#      SHA-256 for the release listing.
#
# `-Install` copies the assembled folder to
# `%LOCALAPPDATA%\Programs\SiliconBoard` and creates a Start Menu entry.
# `-NoBuild` packages the executable already in `target\release`.
#
# The unpacked release folder is a build artefact and is listed in
# .gitignore. The zip is committed at the repo root and published as a
# release asset.

[CmdletBinding()]
param(
    [switch]$Install,
    [switch]$NoBuild
)

$ErrorActionPreference = 'Stop'

$repoRoot = $PSScriptRoot
Push-Location $repoRoot
try {
    $versionLine = Select-String -Path 'Cargo.toml' -Pattern '^version\s*=\s*"([^"]+)"' |
        Select-Object -First 1
    if (-not $versionLine) { throw 'No version found in Cargo.toml.' }
    $version = $versionLine.Matches[0].Groups[1].Value
    $arch = 'x86_64'
    $name = "SiliconBoard-$version-windows-$arch"
    $appDir = Join-Path $repoRoot $name
    $zipPath = Join-Path $repoRoot "$name.zip"

    if (-not $NoBuild) {
        Write-Host '> cargo build --release'
        & cargo build --release
        if ($LASTEXITCODE -ne 0) { throw "cargo build failed with exit code $LASTEXITCODE" }
    }

    $exe = Join-Path $repoRoot 'target\release\siliconboard.exe'
    if (-not (Test-Path $exe)) {
        throw "Missing $exe. Run without -NoBuild to build it first."
    }

    Write-Host "> Assembling $appDir (v$version)"
    if (Test-Path $appDir) { Remove-Item -Recurse -Force $appDir }
    New-Item -ItemType Directory -Force (Join-Path $appDir 'bin') | Out-Null
    Copy-Item $exe (Join-Path $appDir 'bin\siliconboard.exe')
    Copy-Item -Recurse (Join-Path $repoRoot 'public') (Join-Path $appDir 'public')

    # The start-up PGN rides along, so the packaged app also opens
    # PGN\WorldChamp1972.pgn at launch and selects game 1.
    $startupPgn = Join-Path $repoRoot 'PGN\WorldChamp1972.pgn'
    if (Test-Path $startupPgn) {
        New-Item -ItemType Directory -Force (Join-Path $appDir 'PGN') | Out-Null
        Copy-Item $startupPgn (Join-Path $appDir 'PGN\WorldChamp1972.pgn')
    }
    else {
        Write-Warning "$startupPgn is missing - the folder starts without the start-up PGN."
    }

    foreach ($file in @(
            'LICENSE', 'LICENSE-MIT', 'THIRD-PARTY-NOTICES.md',
            'README.md', 'RELEASE_NOTES.md', 'features-list.md')) {
        if (Test-Path (Join-Path $repoRoot $file)) {
            Copy-Item (Join-Path $repoRoot $file) $appDir
        }
    }

    # The asset resolver walks up from bin\siliconboard.exe to the folder that
    # holds public\. Check the packaged layout before shipping it.
    if (-not (Test-Path (Join-Path $appDir 'public\images\board\blue.png'))) {
        throw "The packaged asset tree is incomplete: $appDir\public"
    }
    if (-not (Test-Path (Join-Path $appDir 'public\piece\cburnett\wK.svg'))) {
        throw "The packaged piece set is incomplete: $appDir\public\piece"
    }

    Write-Host "> Building $zipPath"
    if (Test-Path $zipPath) { Remove-Item -Force $zipPath }
    # Build the archive entry by entry so the separators are `/` on every
    # Windows PowerShell version (Compress-Archive stores `\`, which is fine
    # for Explorer but surprising to other unzip tools). A source file can be
    # briefly locked by an indexer or antivirus, so retry a few times.
    Add-Type -AssemblyName System.IO.Compression
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $zipped = $false
    for ($attempt = 1; $attempt -le 5 -and -not $zipped; $attempt++) {
        try {
            if (Test-Path $zipPath) { Remove-Item -Force $zipPath }
            $archive = [System.IO.Compression.ZipFile]::Open(
                $zipPath,
                [System.IO.Compression.ZipArchiveMode]::Create)
            try {
                Get-ChildItem -Path $appDir -Recurse -File | ForEach-Object {
                    $relative = $_.FullName.Substring($appDir.Length).TrimStart([char[]]@('\', '/'))
                    $entryName = "$name/" + $relative.Replace('\', '/')
                    [System.IO.Compression.ZipFileExtensions]::CreateEntryFromFile(
                        $archive,
                        $_.FullName,
                        $entryName,
                        [System.IO.Compression.CompressionLevel]::Optimal) | Out-Null
                }
            }
            finally {
                $archive.Dispose()
            }
            $zipped = $true
        }
        catch {
            if ($attempt -eq 5) { throw }
            Write-Warning "Zip attempt $attempt failed: $($_.Exception.Message); retrying"
            Start-Sleep -Seconds 2
        }
    }

    $hash = (Get-FileHash -Path $zipPath -Algorithm SHA256).Hash.ToLowerInvariant()
    Write-Host "Created $zipPath"
    Write-Host "SHA-256 $hash"

    if ($Install) {
        $target = Join-Path $env:LOCALAPPDATA 'Programs\SiliconBoard'
        Write-Host "> Installing to $target"
        if (Test-Path $target) {
            $running = Get-Process -Name 'siliconboard' -ErrorAction SilentlyContinue
            if ($running) {
                throw 'SiliconBoard is running. Close it and run the script again.'
            }
            Remove-Item -Recurse -Force $target
        }
        New-Item -ItemType Directory -Force $target | Out-Null
        Copy-Item -Recurse (Join-Path $appDir '*') $target

        $startMenu = Join-Path $env:APPDATA 'Microsoft\Windows\Start Menu\Programs'
        New-Item -ItemType Directory -Force $startMenu | Out-Null
        $shortcutPath = Join-Path $startMenu 'SiliconBoard.lnk'
        $shell = New-Object -ComObject WScript.Shell
        $shortcut = $shell.CreateShortcut($shortcutPath)
        $shortcut.TargetPath = Join-Path $target 'bin\siliconboard.exe'
        $shortcut.WorkingDirectory = $target
        $shortcut.Description = 'SiliconBoard - UCI chess GUI'
        $shortcut.Save()
        Write-Host "Installed $shortcutPath"
    }
}
finally {
    Pop-Location
}
