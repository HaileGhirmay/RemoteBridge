# Register the optional native messaging host for the current Windows user.
# Run once after installing rb-native-host.exe. Edge uses its own key; both are written.
#
#   powershell -File tools/install-native-host.ps1 -HostPath "$env:LOCALAPPDATA\RemoteBridge\rb-native-host.exe" -ExtensionId <32-letter id>
#
# The extension id is shown on the store page after publishing, or on
# chrome://extensions when the unpacked extension is loaded in developer mode.
param(
    [Parameter(Mandatory = $true)][string]$HostPath,
    [Parameter(Mandatory = $true)][ValidatePattern('^[a-p]{32}$')][string]$ExtensionId
)
$ErrorActionPreference = 'Stop'

if (-not (Test-Path $HostPath)) { throw "native host not found: $HostPath" }
$hostFull = (Resolve-Path $HostPath).Path

$manifestDir = Join-Path $env:LOCALAPPDATA 'RemoteBridge\native-messaging'
New-Item -ItemType Directory -Force -Path $manifestDir | Out-Null
$manifestPath = Join-Path $manifestDir 'com.remotebridge.host.json'

$manifest = [ordered]@{
    name            = 'com.remotebridge.host'
    description     = 'RemoteBridge host status and open (status and open only)'
    path            = $hostFull
    type            = 'stdio'
    allowed_origins = @("chrome-extension://$ExtensionId/")
}
$manifest | ConvertTo-Json -Depth 4 | Set-Content -Path $manifestPath -Encoding UTF8

$keys = @(
    'HKCU:\Software\Google\Chrome\NativeMessagingHosts\com.remotebridge.host',
    'HKCU:\Software\Microsoft\Edge\NativeMessagingHosts\com.remotebridge.host'
)
foreach ($key in $keys) {
    New-Item -Path $key -Force | Out-Null
    Set-ItemProperty -Path $key -Name '(default)' -Value $manifestPath
}
Write-Output "registered $manifestPath for chrome-extension://$ExtensionId/"
