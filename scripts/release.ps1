<#
.SYNOPSIS
  Publish a release of the AveeCare QuickBooks Connector: one command.

.DESCRIPTION
  1. Checks the working tree is clean and on main.
  2. Sets the version in package.json and src-tauri/Cargo.toml (when -Version is given) and commits it.
  3. The first time only: stores the updater signing key as the repository secret
     TAURI_SIGNING_PRIVATE_KEY (from -KeyPath), so GitHub Actions can sign updates.
  4. Pushes main and the tag v<version>. The "release" workflow builds the
     installers and publishes the GitHub release with latest.json.

.EXAMPLE
  pwsh -File scripts/release.ps1                 # release the version already in package.json
  pwsh -File scripts/release.ps1 -Version 0.2.0  # bump, commit, release

.NOTES
  Needs git and the GitHub CLI (gh) signed in with push rights to aveecare/quickbooks-aveecare-connector.
  The updater signing key is NOT in the repo. Default location: $HOME\.tauri\aveecare-qb-connector.key
  Losing it means installed copies can no longer auto-update (they must be reinstalled by hand).
#>
param(
  [string]$Version,
  [string]$KeyPath = (Join-Path $HOME '.tauri\aveecare-qb-connector.key'),
  [string]$Repo = 'aveecare/quickbooks-aveecare-connector'
)
$ErrorActionPreference = 'Stop'
Set-Location (Split-Path $PSScriptRoot -Parent)

if ((git status --porcelain) -ne $null) { throw 'The working tree has uncommitted changes. Commit or set them aside first.' }
$branch = git rev-parse --abbrev-ref HEAD
if ($branch -ne 'main') { throw "Release from main (you are on $branch)." }

if ($Version) {
  if ($Version -notmatch '^\d+\.\d+\.\d+$') { throw "Version must look like 1.2.3 (got $Version)." }
  $pkg = Get-Content package.json -Raw
  $pkg = $pkg -replace '"version":\s*"[^"]+"', "`"version`": `"$Version`""
  Set-Content package.json $pkg -NoNewline
  $cargo = Get-Content src-tauri/Cargo.toml -Raw
  $cargo = [regex]::Replace($cargo, '(?m)^version = "[^"]+"', "version = `"$Version`"", 1)
  Set-Content src-tauri/Cargo.toml $cargo -NoNewline
  Push-Location src-tauri; cargo update --workspace --offline 2>$null; Pop-Location
  git add package.json src-tauri/Cargo.toml src-tauri/Cargo.lock
  git commit -m "Version $Version"
}
$Version = (Get-Content package.json | ConvertFrom-Json).version
$tag = "v$Version"
if (git tag --list $tag) { throw "Tag $tag already exists. Pass -Version with a new number." }

$secrets = gh secret list --repo $Repo 2>$null
if (-not ($secrets -match '^TAURI_SIGNING_PRIVATE_KEY\s')) {
  if (-not (Test-Path $KeyPath)) { throw "Updater signing key not found at $KeyPath." }
  Get-Content $KeyPath -Raw | gh secret set TAURI_SIGNING_PRIVATE_KEY --repo $Repo
  Write-Host 'Stored the updater signing key as the TAURI_SIGNING_PRIVATE_KEY secret.'
}

git tag -a $tag -m "AveeCare QuickBooks Connector $Version"
git push origin main
git push origin $tag
Write-Host "Pushed $tag. Watch the build: gh run watch --repo $Repo"
Write-Host "Release page: https://github.com/$Repo/releases/tag/$tag"
