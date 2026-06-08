<#
.SYNOPSIS
  Install / update gemini-web2api from the latest GitHub release (Windows).

.EXAMPLE
  iwr -useb https://raw.githubusercontent.com/ducphanvanntq/gemini-web2api/main/scripts/install.ps1 | iex

.PARAMETER Repo
  GitHub "owner/repo". Default: ducphanvanntq/gemini-web2api

.PARAMETER Version
  Release tag. Default: latest

.PARAMETER Prefix
  Install directory. Default: $env:USERPROFILE\.gemini-web2api
#>
[CmdletBinding()]
param(
  [string]$Repo    = $env:REPO,
  [string]$Version = $env:VERSION,
  [string]$Prefix  = $env:PREFIX
)

$ErrorActionPreference = "Stop"

if ([string]::IsNullOrEmpty($Repo))    { $Repo    = "ducphanvanntq/gemini-web2api" }
if ([string]::IsNullOrEmpty($Version)) { $Version = "latest" }
if ([string]::IsNullOrEmpty($Prefix))  { $Prefix  = Join-Path $env:USERPROFILE ".gemini-web2api" }

$arch = $env:PROCESSOR_ARCHITECTURE
if ($arch -ne "AMD64") {
  Write-Error "Unsupported architecture: $arch. Only x86_64 is published."
}
$AssetName = "gemini-web2api-windows-x86_64.zip"

Write-Host "Fetching release info..." -ForegroundColor Cyan
$Headers = @{ "User-Agent" = "gemini-web2api-install" }
if ($Version -eq "latest") {
  $Release = Invoke-RestMethod -Headers $Headers -Uri "https://api.github.com/repos/$Repo/releases/latest"
} else {
  $Release = Invoke-RestMethod -Headers $Headers -Uri "https://api.github.com/repos/$Repo/releases/tags/$Version"
}
$Tag   = $Release.tag_name
$Asset = $Release.assets | Where-Object { $_.name -eq $AssetName }
if (-not $Asset) {
  Write-Host "Release asset '$AssetName' not found in $Tag." -ForegroundColor Red
  Write-Host "Have you published a release yet? (Actions -> release -> Run workflow)" -ForegroundColor Yellow
  exit 1
}

Write-Host "Downloading $AssetName ($Tag)..." -ForegroundColor Cyan
$TmpZip = Join-Path $env:TEMP "gemini-web2api.zip"
$TmpDir = Join-Path $env:TEMP "gemini-web2api-extract"
if (Test-Path $TmpDir) { Remove-Item $TmpDir -Recurse -Force }
Invoke-WebRequest -Uri $Asset.browser_download_url -OutFile $TmpZip -UseBasicParsing
Expand-Archive -Path $TmpZip -DestinationPath $TmpDir -Force

# Windows lock workaround: if the exe is currently running we can't overwrite,
# but renaming is allowed.
$ExePath = Join-Path $Prefix "gemini-web2api.exe"
$OldExe  = Join-Path $Prefix "gemini-web2api.old.exe"
if (Test-Path $ExePath) {
  if (Test-Path $OldExe) { Remove-Item $OldExe -Force -ErrorAction SilentlyContinue }
  Rename-Item $ExePath $OldExe -Force -ErrorAction SilentlyContinue
}

$ExtractedExe = Get-ChildItem -Path $TmpDir -Recurse -Filter "gemini-web2api.exe" | Select-Object -First 1
if (-not $ExtractedExe) { Write-Error "gemini-web2api.exe not found in the archive." }

if (-not (Test-Path $Prefix)) { New-Item -ItemType Directory -Path $Prefix | Out-Null }
Copy-Item -Force -Path $ExtractedExe.FullName -Destination $ExePath

# Drop a default config where the binary auto-discovers it, unless one exists.
$ConfigDir  = Join-Path $env:USERPROFILE ".config\gemini-web2api"
$ConfigFile = Join-Path $ConfigDir "config.json"
$Example    = Get-ChildItem -Path $TmpDir -Recurse -Filter "config.example.json" | Select-Object -First 1
if ($Example -and -not (Test-Path $ConfigFile)) {
  if (-not (Test-Path $ConfigDir)) { New-Item -ItemType Directory -Path $ConfigDir -Force | Out-Null }
  Copy-Item -Force -Path $Example.FullName -Destination $ConfigFile
  Write-Host "Default config written: $ConfigFile" -ForegroundColor Green
}

# Cleanup
Remove-Item $TmpZip -Force -ErrorAction SilentlyContinue
Remove-Item $TmpDir -Recurse -Force -ErrorAction SilentlyContinue

# Add to user PATH
$UserPath = [Environment]::GetEnvironmentVariable("Path", "User")
if ($UserPath -and ($UserPath.Split(";") -contains $Prefix)) {
  Write-Host "$Prefix is already in PATH." -ForegroundColor Yellow
} else {
  $NewPath = if ([string]::IsNullOrEmpty($UserPath)) { $Prefix } else { "$UserPath;$Prefix" }
  [Environment]::SetEnvironmentVariable("Path", $NewPath, "User")
  Write-Host "Added $Prefix to PATH. Restart your terminal for changes to take effect." -ForegroundColor Green
}

Write-Host ""
Write-Host "Done! gemini-web2api $Tag installed to $Prefix" -ForegroundColor Green
Write-Host "  - $ExePath"
Write-Host ""
Write-Host "Run the server with:  gemini-web2api" -ForegroundColor Cyan
Write-Host "It listens on http://localhost:8081/v1 by default." -ForegroundColor Cyan
Write-Host "Edit $ConfigFile to set api_keys / port / cookie, or pass --port / --config." -ForegroundColor Cyan
