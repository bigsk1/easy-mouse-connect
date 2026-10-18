[CmdletBinding()]
param([Parameter(Mandatory = $true)][string] $BinaryPath)

. (Join-Path $PSScriptRoot 'common.ps1')
Assert-Windows
$binary = (Resolve-Path -LiteralPath $BinaryPath).ProviderPath
if ([System.IO.Path]::GetFileName($binary) -ine 'easy-mouse-connect.exe') {
    throw 'Choose the exact easy-mouse-connect.exe build to test.'
}
$existing = Get-Process -Name 'easy-mouse-connect' -ErrorAction SilentlyContinue
$listener = Get-NetTCPConnection -LocalPort 24859 -State Listen -ErrorAction SilentlyContinue
if ($existing -or $listener) {
    throw 'An app process or TCP 24859 listener is already running. Quit it yourself before this isolated startup check.'
}
$identity = Join-Path $env:LOCALAPPDATA 'EasyMouseConnect\identity.dat'
if (Test-Path -LiteralPath $identity) {
    throw 'This account already has an app identity. Use a fresh Windows test account so the smoke check cannot load a real pairing.'
}

$process = Start-Process -FilePath $binary -PassThru
try {
    $ready = $false
    for ($attempt = 0; $attempt -lt 50; $attempt++) {
        Start-Sleep -Milliseconds 200
        $process.Refresh()
        if ($process.HasExited) { throw "Application exited during startup: $($process.ExitCode)" }
        if ($process.MainWindowHandle -ne [IntPtr]::Zero) {
            $listener = Get-NetTCPConnection -LocalPort 24859 -State Listen -ErrorAction SilentlyContinue |
                Where-Object { $_.OwningProcess -eq $process.Id }
            if ($listener) { $ready = $true; break }
        }
    }
    if (-not $ready) { throw 'Application did not present a window and own its TCP listener within ten seconds.' }
    Write-Host 'PASS: native process, visible Windows window, and TCP 24859 listener.'
    $null = $process.CloseMainWindow()
    Start-Sleep -Milliseconds 700
    $process.Refresh()
    if ($process.HasExited) { throw 'Closing the window unexpectedly exited the tray application.' }
    if ($process.MainWindowHandle -ne [IntPtr]::Zero) { throw 'Closing the window did not hide it in the tray.' }
    Write-Host 'PASS: closing the window keeps the tray application running.'
    Write-Host 'Input forwarding, clipboard transfer and two-PC behavior were not exercised by this smoke check.'
} finally {
    $process.Refresh()
    if (-not $process.HasExited) {
        # This check owns only this exact child process. Tray Quit and graceful
        # input release require the separate interactive acceptance case.
        Stop-Process -Id $process.Id
    }
}
