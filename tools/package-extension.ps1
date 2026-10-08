# Build the extension package for the Chrome Web Store and Edge Add-ons.
# Runs the local store checks first and refuses to package when they fail.
#   powershell -File tools/package-extension.ps1
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

node tools/extension-lint.mjs extension
if ($LASTEXITCODE -ne 0) { throw 'extension lint failed; not packaging' }
node --test extension/test/lib.test.mjs | Out-Null
if ($LASTEXITCODE -ne 0) { throw 'extension tests failed; not packaging' }

$manifest = Get-Content extension/manifest.json -Raw | ConvertFrom-Json
$staging = Join-Path $env:TEMP "rb-extension-$($manifest.version)"
if (Test-Path $staging) { Remove-Item $staging -Recurse -Force }
New-Item -ItemType Directory -Path $staging | Out-Null

# Only what the browser needs. No tests, no package.json used for node, no store notes.
Copy-Item extension/manifest.json, extension/popup.html, extension/popup.css -Destination $staging
Copy-Item extension/icons -Destination $staging -Recurse
New-Item -ItemType Directory -Path (Join-Path $staging 'src') | Out-Null
Copy-Item extension/src/lib.js, extension/src/popup.js -Destination (Join-Path $staging 'src')

$dist = Join-Path $root 'dist'
New-Item -ItemType Directory -Force -Path $dist | Out-Null
$zip = Join-Path $dist "remotebridge-extension-$($manifest.version).zip"
if (Test-Path $zip) { Remove-Item $zip -Force }
Compress-Archive -Path (Join-Path $staging '*') -DestinationPath $zip
Remove-Item $staging -Recurse -Force

Write-Output "packaged $zip"
