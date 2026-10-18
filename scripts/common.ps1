Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function Get-RepositoryRoot {
    return (Split-Path -Parent $PSScriptRoot)
}

function Assert-Windows {
    if ($env:OS -ne 'Windows_NT') {
        throw 'Use native Windows PowerShell for the desktop scripts. Portable core commands are in README.md.'
    }
}

function Invoke-Checked {
    param(
        [Parameter(Mandatory = $true)][string] $Command,
        [string[]] $Arguments = @()
    )
    & $Command @Arguments
    if ($LASTEXITCODE -ne 0) {
        throw "$Command failed with exit code $LASTEXITCODE."
    }
}

function Get-TauriCli {
    $cli = Join-Path (Get-RepositoryRoot) 'node_modules\.bin\tauri.cmd'
    if (-not (Test-Path -LiteralPath $cli -PathType Leaf)) {
        throw 'Tauri CLI is missing. Run scripts\setup-windows.ps1 -InstallDevDependencies first.'
    }
    return $cli
}

