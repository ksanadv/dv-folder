# Releases are automatic: the Release workflow (.github/workflows/release.yml) publishes a release whenever main has a
# version in Cargo.toml that has no release yet. So a release is just a version change that goes through a branch like
# any other change; nothing is committed to main and no tag is made by hand.
#   .\release.ps1 -Bump 0.2.0   sets the version in both Cargo.toml files (and their lock files), builds and checks it
#   .\release.ps1               shows the current version and the steps
param([string]$Bump)
$ErrorActionPreference = 'Stop'
Set-Location $PSScriptRoot
$pattern = [regex]'(?m)^version\s*=\s*"([^"]+)"'

if (-not $Bump) {
    $version = $pattern.Match([IO.File]::ReadAllText((Join-Path $PSScriptRoot 'Cargo.toml'))).Groups[1].Value
    "Current version: $version"
    "To release a new one: git switch -c release-X.Y.Z; .\release.ps1 -Bump X.Y.Z; commit; open a pull request and merge it."
    "GitHub Actions then builds, tests and publishes v<version> by itself (watch the Actions tab)."
    exit 0
}

if ($Bump -notmatch '^\d+\.\d+\.\d+$') { throw "Use a version like 1.2.3, not '$Bump'" }
if ((git branch --show-current) -eq 'main') { throw 'Do not bump on main: make a branch first (git switch -c release-' + $Bump + ')' }
foreach ($f in 'Cargo.toml', 'installer/Cargo.toml') {
    $path = Join-Path $PSScriptRoot $f
    [IO.File]::WriteAllText($path, $pattern.Replace([IO.File]::ReadAllText($path), "version = `"$Bump`"", 1))
}
# also brings both Cargo.lock files up to the new version, and proves the release builds
.\build-installer.ps1
if ($LASTEXITCODE) { exit $LASTEXITCODE }
cargo test --release
if ($LASTEXITCODE) { exit $LASTEXITCODE }
# the identity on the commit must be the pseudonym (see ..\README.md, "Workflow")
if ((git config user.email) -notmatch '@users\.noreply\.github\.com$') { Write-Warning 'This repo''s git user.email is not the pseudonym; set it before you commit.' }
"Version set to $Bump. Commit it on this branch, open a pull request and merge it; the release is then published automatically."
