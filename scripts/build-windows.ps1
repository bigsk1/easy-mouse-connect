[CmdletBinding()]
param([switch] $SkipChecks)

. (Join-Path $PSScriptRoot 'common.ps1')
Assert-Windows
$cli = Get-TauriCli
if (-not $SkipChecks) {
    & (Join-Path $PSScriptRoot 'check.ps1')
}
Push-Location (Join-Path (Get-RepositoryRoot) 'apps\desktop')
try {
    Invoke-Checked -Command $cli -Arguments @('build', '--bundles', 'nsis', '--', '--locked')
} finally {
    Pop-Location
}
Write-Host 'Unsigned development installer: target\release\bundle\nsis\'

