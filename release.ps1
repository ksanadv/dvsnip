# Releases never commit to main; the version change goes through a branch like any other change.
#   .\release.ps1 -Bump 0.2.0   sets the version in both Cargo.toml files (and their lock files) on the current branch
#   .\release.ps1               on an up-to-date main: builds and tests, then tags v<version> and pushes only the tag.
#                               GitHub Actions (.github/workflows/release.yml) builds and publishes the release from it.
param([string]$Bump)
$ErrorActionPreference = 'Stop'
Set-Location $PSScriptRoot
$pattern = [regex]'(?m)^version\s*=\s*"([^"]+)"'

if ($Bump) {
    if ($Bump -notmatch '^\d+\.\d+\.\d+$') { throw "Use a version like 1.2.3, not '$Bump'" }
    foreach ($f in 'Cargo.toml', 'installer/Cargo.toml') {
        $path = Join-Path $PSScriptRoot $f
        [IO.File]::WriteAllText($path, $pattern.Replace([IO.File]::ReadAllText($path), "version = `"$Bump`"", 1))
    }
    # also brings both Cargo.lock files up to the new version
    .\build-installer.ps1
    if ($LASTEXITCODE) { exit $LASTEXITCODE }
    "Version set to $Bump. Commit it on a branch and merge it, then run .\release.ps1 on main."
    exit 0
}

$version = $pattern.Match([IO.File]::ReadAllText((Join-Path $PSScriptRoot 'Cargo.toml'))).Groups[1].Value
$tag = "v$version"
if ((git branch --show-current) -ne 'main') { throw 'Switch to main first' }
if (git status --porcelain) { throw 'Commit or stash your changes first' }
# the identity on the tag and on every commit must be the pseudonym (see ..\README.md, "Workflow")
if ((git config user.email) -notmatch '@users\.noreply\.github\.com$') { throw 'Set this repo''s git user.name/user.email to the pseudonym first' }
git fetch origin main --tags
if ((git rev-parse HEAD) -ne (git rev-parse origin/main)) { throw 'main is not the same as origin/main; pull first' }
if (git tag --list $tag) { throw "$tag already exists; bump the version with .\release.ps1 -Bump" }

.\build-installer.ps1
if ($LASTEXITCODE) { exit $LASTEXITCODE }
cargo test --release
if ($LASTEXITCODE) { exit $LASTEXITCODE }

git tag -a $tag -m "dvsnip $version"
git push origin $tag
if ($LASTEXITCODE) { exit $LASTEXITCODE }
"Pushed $tag. The release appears at https://github.com/ksanadv/dvsnip/releases once the Release workflow finishes."
