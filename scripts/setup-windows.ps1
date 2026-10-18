[CmdletBinding()]
param([switch] $InstallDevDependencies)

. (Join-Path $PSScriptRoot 'common.ps1')
Assert-Windows

foreach ($command in @('rustup', 'cargo', 'rustc', 'node', 'npm.cmd')) {
    if (-not (Get-Command $command -ErrorAction SilentlyContinue)) {
        throw "Missing $command. Install the Windows prerequisites listed in README.md, then reopen this terminal."
    }
}

$rustInfo = (& rustc -vV | Out-String)
if ($LASTEXITCODE -ne 0 -or $rustInfo -notmatch 'host: .*pc-windows-msvc') {
    throw 'Select the native Windows MSVC Rust toolchain. See rustup default stable-msvc and README.md.'
}
if ($rustInfo -notmatch 'release: ([0-9]+\.[0-9]+\.[0-9]+)' -or [version] $Matches[1] -lt [version] '1.99.0') {
    throw 'This revision requires Rust 1.99 or later. Update the native MSVC toolchain with rustup update stable.'
}
$nodeVersion = (& node -p 'process.versions.node').Trim()
if ($LASTEXITCODE -ne 0 -or [version] $nodeVersion -lt [version] '22.0.0') {
    throw 'Install Node.js LTS version 22 or later, then reopen this terminal.'
}

$components = (& rustup component list --installed | Out-String)
if ($LASTEXITCODE -ne 0 -or $components -notmatch 'rustfmt' -or $components -notmatch 'clippy') {
    throw 'Install Rust check tools with: rustup component add rustfmt clippy'
}

Write-Host $rustInfo.Trim()
Write-Host "Node.js $nodeVersion"
Write-Host 'Ensure Visual Studio C++ Build Tools, a Windows SDK and WebView2 Runtime are installed; README.md links the official prerequisites.'

if ($InstallDevDependencies) {
    Push-Location (Get-RepositoryRoot)
    try {
        if (Test-Path -LiteralPath 'package-lock.json') {
            Invoke-Checked -Command 'npm.cmd' -Arguments @('ci')
        } else {
            Invoke-Checked -Command 'npm.cmd' -Arguments @('install')
        }
    } finally {
        Pop-Location
    }
}

Write-Host 'Environment checks passed. Run scripts\check.ps1, then scripts\dev.ps1.'

