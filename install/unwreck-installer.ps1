# Install the latest GitHub Release binary on Windows.
$ErrorActionPreference = "Stop"
$repo = if ($env:UNWRECK_REPO) { $env:UNWRECK_REPO } else { "ayushdoesdev/unwreck" }
$prefix = if ($env:UNWRECK_PREFIX) { $env:UNWRECK_PREFIX } else { "$env:LOCALAPPDATA\unwreck" }
$t = "x86_64-pc-windows-msvc"
$url = "https://github.com/$repo/releases/latest/download/unwreck-$t.zip"
$zip = Join-Path $env:TEMP "unwreck.zip"
Invoke-WebRequest -Uri $url -OutFile $zip
$dest = Join-Path $env:TEMP "unwreck-unpack"
if (Test-Path $dest) { Remove-Item -Recurse -Force $dest }
Expand-Archive -Path $zip -DestinationPath $dest
New-Item -ItemType Directory -Force -Path $prefix | Out-Null
Copy-Item -Force (Join-Path $dest "unwreck-$t\unwreck.exe") (Join-Path $prefix "unwreck.exe")
Write-Host "installed $(Join-Path $prefix 'unwreck.exe')"
Write-Host "Add $prefix to PATH if needed."
