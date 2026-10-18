[CmdletBinding()]
param()

. (Join-Path $PSScriptRoot 'common.ps1')
Assert-Windows
$cli = Get-TauriCli
Push-Location (Join-Path (Get-RepositoryRoot) 'apps\desktop')
try {
    Invoke-Checked -Command $cli -Arguments @('dev')
} finally {
    Pop-Location
}

