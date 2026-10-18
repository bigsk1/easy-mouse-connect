[CmdletBinding()]
param([switch] $CoreOnly)

. (Join-Path $PSScriptRoot 'common.ps1')
Push-Location (Get-RepositoryRoot)
try {
    Invoke-Checked -Command 'cargo' -Arguments @('fmt', '--all', '--', '--check')
    $packages = @('-p', 'emc-protocol', '-p', 'emc-core', '-p', 'emc-platform')
    Invoke-Checked -Command 'cargo' -Arguments (@('test', '--locked') + $packages)
    if ($env:OS -eq 'Windows_NT') {
        & (Join-Path $PSScriptRoot 'test-native-input.ps1')
    }
    Invoke-Checked -Command 'cargo' -Arguments (@('clippy', '--locked') + $packages + @('--all-targets', '--', '-D', 'warnings'))
    Invoke-Checked -Command 'node' -Arguments @('--check', 'ui/app.js')
    if (-not $CoreOnly) {
        Assert-Windows
        Invoke-Checked -Command 'cargo' -Arguments @('check', '--locked', '--manifest-path', 'apps/desktop/Cargo.toml')
        Invoke-Checked -Command 'cargo' -Arguments @('clippy', '--locked', '--manifest-path', 'apps/desktop/Cargo.toml', '--all-targets', '--', '-D', 'warnings')
    }
} finally {
    Pop-Location
}

