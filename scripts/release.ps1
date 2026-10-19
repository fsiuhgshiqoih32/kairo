<#
.SYNOPSIS
  Publishes a new Kairo version in one step.

.DESCRIPTION
  Checks that main is clean and up to date and that CHANGELOG.md has release notes for the
  version, sets the version in Cargo.toml and Cargo.lock, commits "Kairo <version>" and pushes.
  GitHub then tests it, builds Windows, macOS and Linux, and publishes the release. Installed
  copies show an update notice within six hours (at once when their window opens).

.EXAMPLE
  ./scripts/release.ps1 0.3.0

.EXAMPLE
  ./scripts/release.ps1 0.3.1 -DryRun     # check everything, change nothing
#>
param(
    [Parameter(Mandatory = $true, Position = 0)][string]$Version,
    [switch]$DryRun
)
$ErrorActionPreference = 'Stop'
Set-Location (Split-Path $PSScriptRoot -Parent)

function Stop-With([string]$why) {
    Write-Host "Not released: $why" -ForegroundColor Red
    exit 1
}

if ($Version -notmatch '^\d+\.\d+\.\d+$') { Stop-With "use a version like 0.3.0 (got '$Version')." }

$cargo = Get-Content Cargo.toml -Raw
if ($cargo -notmatch '(?m)^version = "(\d+\.\d+\.\d+)"') { Stop-With "Cargo.toml has no version line." }
$current = $Matches[1]
if ([version]$Version -le [version]$current) { Stop-With "$Version isn't newer than the current $current." }

# The release has to be exactly what's on GitHub's main branch.
$branch = (git rev-parse --abbrev-ref HEAD).Trim()
if ($branch -ne 'main') { Stop-With "switch to main first (you're on $branch)." }
if (git status --porcelain --untracked-files=no) { Stop-With "commit or stash your changes first." }
git fetch origin main --quiet
if ((git rev-parse HEAD).Trim() -ne (git rev-parse origin/main).Trim()) {
    Stop-With "main differs from GitHub's: pull (or push) first."
}

# Release notes: the app shows them in its update notice and "What's new", GitHub on the release.
$log = Get-Content CHANGELOG.md -Raw
if ($log -notmatch "(?m)^## $([regex]::Escape($Version))\s*$") {
    Stop-With "CHANGELOG.md has no '## $Version' section. Write the release notes there first: one '- **Title.** What it does.' bullet per change."
}

Write-Host "Kairo $current -> $Version"
if ($DryRun) {
    Write-Host "Dry run: everything checks out. Nothing was changed." -ForegroundColor Green
    exit 0
}

# Only the package's own version line (dependencies keep theirs).
$line = [regex]'(?m)^version = "\d+\.\d+\.\d+"'
$cargo = $line.Replace($cargo, "version = `"$Version`"", 1)
[System.IO.File]::WriteAllText((Resolve-Path Cargo.toml), $cargo)
# Cargo writes the new version into Cargo.lock without updating any dependency.
cargo metadata --format-version 1 | Out-Null
if ($LASTEXITCODE -ne 0) { Stop-With "cargo couldn't update Cargo.lock." }

git add Cargo.toml Cargo.lock
git commit --quiet -m "Kairo $Version"
git push --quiet origin main
if ($LASTEXITCODE -ne 0) { Stop-With "the push failed; the version commit is still here, push it when you can." }

Write-Host ""
Write-Host "Pushed Kairo $Version." -ForegroundColor Green
Write-Host "GitHub now tests it, builds every platform and publishes the release (about 30 minutes):"
Write-Host "  https://github.com/fsiuhgshiqoih32/kairo/actions"
Write-Host "Then installed copies show the update notice within six hours, or at once when their window opens."
