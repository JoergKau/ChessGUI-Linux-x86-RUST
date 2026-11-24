# Regenerating `SHA256SUMS`

Run these commands in PowerShell from the repository root
(`C:\dev\SiliconBoard-ChessGUI`). Do this **after the final build**: a rebuilt
zip gets new timestamps, so its SHA-256 changes every time.

The file format is the standard `sha256sum` form, with **two spaces** between
the hash and the file name:

```text
<hash>  <file>
```

## Replace only the Windows line

```powershell
$zip  = "SiliconBoard-1.4.1-windows-x86_64.zip"
$hash = (Get-FileHash $zip -Algorithm SHA256).Hash.ToLowerInvariant()

$lines = @(Get-Content SHA256SUMS | Where-Object { $_ -notlike "*$zip" })
$lines += "$hash  $zip"
$lines | Set-Content SHA256SUMS -Encoding ASCII
```

## Regenerate the whole file (recommended)

This rewrites every entry in the existing order:

```powershell
$files = @(
    "SiliconBoard-1.4.1-macos-arm64.zip",
    "SiliconBoard-1.4.1-linux-x86_64.tar.gz",
    "SiliconBoard-1.4.1-windows-x86_64.zip"
)

$files | ForEach-Object {
    "$((Get-FileHash $_ -Algorithm SHA256).Hash.ToLowerInvariant())  $_"
} | Set-Content SHA256SUMS -Encoding ASCII
```

## Verify the result

```powershell
Get-Content SHA256SUMS

Get-Content SHA256SUMS | ForEach-Object {
    $hash, $file = $_ -split '\s+', 2
    $actual = (Get-FileHash $file -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($hash -eq $actual) { "OK   $file" } else { "FAIL $file" }
}
```

In Git Bash or WSL the standard checker also works:

```bash
sha256sum -c SHA256SUMS
```

## Stage the changed file

```powershell
git add SHA256SUMS
git status --short
```
