[CmdletBinding()]
param([Parameter(Mandatory = $true)][string] $PackageDirectory)

. (Join-Path $PSScriptRoot 'common.ps1')
Assert-Windows
$package = (Resolve-Path -LiteralPath $PackageDirectory).ProviderPath
$config = Get-Content -LiteralPath (Join-Path (Get-RepositoryRoot) 'apps\desktop\tauri.conf.json') -Raw | ConvertFrom-Json
$installer = Join-Path $package ("EasyMouseConnect-{0}-preview-setup.exe" -f $config.version)
$original = Join-Path $package 'easy-mouse-connect.exe'
$install = Join-Path (Get-RepositoryRoot) 'artifacts\installer-smoke'
$uninstallKey = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\EasyMouseConnectPreview'
if ((Test-Path -LiteralPath $install) -or (Test-Path -LiteralPath $uninstallKey)) {
    throw 'An existing preview installation or smoke-test folder is present. Refusing to overwrite it.'
}
$identity = Join-Path $env:LOCALAPPDATA 'EasyMouseConnect\identity.dat'
if ((Get-Process -Name 'easy-mouse-connect' -ErrorAction SilentlyContinue) -or
    (Get-NetTCPConnection -LocalPort 24859 -State Listen -ErrorAction SilentlyContinue) -or
    (Test-Path -LiteralPath $identity)) {
    throw 'An existing app, listener or identity is present. Use a fresh Windows test account; no installation was changed.'
}

$process = Start-Process -FilePath $installer -ArgumentList "/S /D=$install" -Wait -PassThru
if ($process.ExitCode -ne 0) { throw "Installer failed with exit code $($process.ExitCode)" }
try {
    $installed = Join-Path $install 'easy-mouse-connect.exe'
    if ((Get-FileHash -LiteralPath $original).Hash -ne (Get-FileHash -LiteralPath $installed).Hash) {
        throw 'Installed executable differs from the supplied build.'
    }
    $registered = (Get-ItemProperty -LiteralPath $uninstallKey).UninstallString
    if ($registered -ne ('"' + (Join-Path $install 'Uninstall.exe') + '"')) {
        throw 'The per-user uninstall registration has an invalid command.'
    }
    & (Join-Path $PSScriptRoot 'smoke-windows.ps1') -BinaryPath $installed
    Write-Host 'PASS: per-user preview installation and installed executable identity.'
} finally {
    $uninstaller = Join-Path $install 'Uninstall.exe'
    if (Test-Path -LiteralPath $uninstaller) {
        $null = Start-Process -FilePath $uninstaller -ArgumentList '/S' -Wait -PassThru
        for ($attempt = 0; $attempt -lt 50; $attempt++) {
            if (-not (Test-Path -LiteralPath $uninstallKey) -and -not (Test-Path -LiteralPath (Join-Path $install 'easy-mouse-connect.exe'))) { break }
            Start-Sleep -Milliseconds 200
        }
    }
}
if ((Test-Path -LiteralPath $uninstallKey) -or (Test-Path -LiteralPath (Join-Path $install 'easy-mouse-connect.exe'))) {
    throw 'Preview uninstaller did not remove its application and per-user registration.'
}
Write-Host 'PASS: preview uninstall removed application and registration; user data preserved.'
