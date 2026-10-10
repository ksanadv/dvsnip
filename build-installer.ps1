$ErrorActionPreference = 'Stop'
Set-Location $PSScriptRoot

$pattern = '^version\s*=\s*"([^"]+)"'
$appVersion = (Select-String -Path Cargo.toml -Pattern $pattern | Select-Object -First 1).Matches.Groups[1].Value
$setupVersion = (Select-String -Path installer/Cargo.toml -Pattern $pattern | Select-Object -First 1).Matches.Groups[1].Value
if ($appVersion -ne $setupVersion) { throw "Cargo.toml is $appVersion but installer/Cargo.toml is $setupVersion; keep them equal" }

cargo build --release
if ($LASTEXITCODE) { exit $LASTEXITCODE }
$target = (cargo metadata --format-version 1 --no-deps | ConvertFrom-Json).target_directory
$env:DVSNIP_EXE = Join-Path $target 'release\dvsnip.exe'
$env:DVSNIP_CLI_EXE = Join-Path $target 'release\dvsnip-cli.exe'

cargo build --release --manifest-path installer/Cargo.toml
if ($LASTEXITCODE) { exit $LASTEXITCODE }

New-Item -ItemType Directory -Force dist | Out-Null
Copy-Item (Join-Path $target 'release\dvsnip-setup.exe') dist\dvsnip-Setup.exe -Force
Get-Item dist\dvsnip-Setup.exe | ForEach-Object { '{0}  {1:N1} MB' -f $_.FullName, ($_.Length / 1MB) }
