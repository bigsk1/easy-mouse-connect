[CmdletBinding()]
param()

. (Join-Path $PSScriptRoot 'common.ps1')
Assert-Windows
Push-Location (Get-RepositoryRoot)
try {
    # These ignored tests create only their own hidden native windows and
    # process-local registrations. No hooks, injection, clipboard or app profile.
    $filter = 'windows::tests::native_raw_registration_'
    $listing = & cargo test --locked -p emc-platform $filter -- --ignored --list
    if ($LASTEXITCODE -ne 0) {
        throw 'Unable to build or list the native raw-input contract tests.'
    }
    $contracts = @($listing | Where-Object { $_ -match '^windows::tests::native_raw_registration_[A-Za-z0-9_]+: test$' })
    if ($contracts.Count -lt 2) {
        throw 'Expected at least two native raw-input contracts; refusing an empty/partial test selection.'
    }
    Invoke-Checked -Command 'cargo' -Arguments @(
        'test', '--locked', '-p', 'emc-platform', $filter,
        '--', '--ignored', '--test-threads=1', '--nocapture'
    )
} finally {
    Pop-Location
}
