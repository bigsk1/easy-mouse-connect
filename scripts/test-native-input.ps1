[CmdletBinding()]
param()

. (Join-Path $PSScriptRoot 'common.ps1')
Assert-Windows
Push-Location (Get-RepositoryRoot)
try {
    # Separate test process: owned hidden windows, synthetic commands/messages
    # and process-local raw registrations. No hooks, SendInput or app profile.
    $filter = 'windows::tests::native_'
    $expected = @(
        'native_input_pump_coalesces_wakes_and_preserves_original_message_order',
        'native_input_command_preserves_original_deadline_before_synthetic_execution',
        'native_raw_registration_roundtrip_normalizes_flags_for_eleven_seconds',
        'native_raw_registration_rejects_replacement_target_and_missing_sink',
        'native_source_edge_raw_first_clamped_cursor_uses_production_admission',
        'native_injection_executor_pumps_owner_messages_and_releases_cancelled_holds'
    )
    $listing = & cargo test --locked -p emc-platform $filter -- --ignored --list
    if ($LASTEXITCODE -ne 0) { throw 'Unable to build or list native input contracts.' }
    foreach ($name in $expected) {
        $entry = "windows::tests::${name}: test"
        if ($listing -notcontains $entry) {
            throw "Missing native contract '$name'; refusing a partial test run."
        }
    }
    Invoke-Checked -Command 'cargo' -Arguments @(
        'test', '--locked', '-p', 'emc-platform', $filter,
        '--', '--ignored', '--test-threads=1', '--nocapture'
    )
} finally {
    Pop-Location
}
