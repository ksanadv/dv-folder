# 'Continue' so Windows PowerShell 5.1 does not treat cargo's progress on stderr as a failure; exit codes are checked
$ErrorActionPreference = 'Continue'
Set-Location $PSScriptRoot

$pattern = '^version\s*=\s*"([^"]+)"'
$appVersion = (Select-String -Path Cargo.toml -Pattern $pattern | Select-Object -First 1).Matches.Groups[1].Value
$setupVersion = (Select-String -Path installer/Cargo.toml -Pattern $pattern | Select-Object -First 1).Matches.Groups[1].Value
if ($appVersion -ne $setupVersion) { throw "Cargo.toml is $appVersion but installer/Cargo.toml is $setupVersion; keep them equal" }

cargo build --release
if ($LASTEXITCODE) { exit $LASTEXITCODE }
$target = (cargo metadata --format-version 1 --no-deps | ConvertFrom-Json).target_directory
$env:DVFILES_EXE = Join-Path $target 'release\dvfiles.exe'
$env:DVFILES_CLI = Join-Path $target 'release\dvfiles-cli.exe'

# the installer is its own workspace, so point it at the same target directory
cargo build --release --manifest-path installer/Cargo.toml --target-dir $target
if ($LASTEXITCODE) { exit $LASTEXITCODE }

$name = "dvfiles-$appVersion-windows-x64"
New-Item -ItemType Directory -Force dist | Out-Null
Remove-Item dist\dvfiles-* -ErrorAction SilentlyContinue
Copy-Item (Join-Path $target 'release\dvfiles-setup.exe') "dist\$name-setup.exe"
Copy-Item (Join-Path $target 'release\dvfiles.exe') "dist\$name-portable.exe"
Copy-Item (Join-Path $target 'release\dvfiles-cli.exe') "dist\$name-cli.exe"

# the exes must not need the Visual C++ runtime, which is what `+crt-static` in .cargo/config.toml is for. That file is
# not in the repo (it holds a machine path), so a build without it would otherwise pass unnoticed.
foreach ($f in Get-Item dist\$name-*) {
    $text = [Text.Encoding]::ASCII.GetString([IO.File]::ReadAllBytes($f.FullName))
    if ($text -match '(?i)vcruntime\d*\.dll') { throw "$($f.Name) imports the Visual C++ runtime; build with -C target-feature=+crt-static (see .cargo/config.toml.example)" }
}
Get-Item dist\$name-* | ForEach-Object { '{0}  {1:N1} MB' -f $_.FullName, ($_.Length / 1MB) }
