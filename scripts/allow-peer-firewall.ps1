[CmdletBinding(SupportsShouldProcess = $true, ConfirmImpact = 'Medium')]
param(
    [Parameter(Mandatory = $true)][string] $ProgramPath,
    [Parameter(Mandatory = $true)][string] $PeerAddress,
    [ValidateRange(1024, 65535)][int] $Port = 24859
)

if ($env:OS -ne 'Windows_NT') {
    throw 'This firewall helper requires Windows.'
}
# Import the OS module explicitly rather than resolving a same-named module
# from a user directory. The application's elevated broker also isolates its
# process-local PSModulePath before entering this script.
Import-Module "$PSHOME\Modules\NetSecurity\NetSecurity.psd1" -Force -ErrorAction Stop

$peerIp = $null
if (-not [System.Net.IPAddress]::TryParse($PeerAddress, [ref] $peerIp)) {
    throw 'PeerAddress must be one literal IPv4 or IPv6 address. Wildcards, DNS names and subnets are not accepted.'
}
if ($peerIp.IsIPv4MappedToIPv6) {
    $peerIp = $peerIp.MapToIPv4()
}
if ([System.Net.IPAddress]::Any.Equals($peerIp) -or [System.Net.IPAddress]::IPv6Any.Equals($peerIp) -or
    [System.Net.IPAddress]::Broadcast.Equals($peerIp) -or $peerIp.IsIPv6Multicast -or
    ($peerIp.AddressFamily -eq [System.Net.Sockets.AddressFamily]::InterNetwork -and $peerIp.GetAddressBytes()[0] -ge 224)) {
    throw 'PeerAddress must be a single unicast peer address.'
}
if (-not (Test-Path -LiteralPath $ProgramPath -PathType Leaf)) {
    throw 'ProgramPath must point to the existing easy-mouse-connect.exe you intend to allow.'
}
$resolvedProgram = (Resolve-Path -LiteralPath $ProgramPath).ProviderPath
if ([System.IO.Path]::GetFileName($resolvedProgram) -ine 'easy-mouse-connect.exe') {
    throw 'ProgramPath must name easy-mouse-connect.exe.'
}

$identity = [System.Security.Principal.WindowsIdentity]::GetCurrent()
$principal = New-Object System.Security.Principal.WindowsPrincipal($identity)
if (-not $principal.IsInRole([System.Security.Principal.WindowsBuiltInRole]::Administrator) -and -not $WhatIfPreference) {
    throw 'Adding a firewall rule requires an Administrator PowerShell terminal. Use -WhatIf to preview without changing policy.'
}

$normalizedIp = $peerIp.ToString()
$hashAlgorithm = [System.Security.Cryptography.SHA256]::Create()
try {
    $ruleIdentity = "$resolvedProgram|$normalizedIp|$Port"
    $hashBytes = $hashAlgorithm.ComputeHash([System.Text.Encoding]::UTF8.GetBytes($ruleIdentity))
    $suffix = ([BitConverter]::ToString($hashBytes)).Replace('-', '').Substring(0, 16)
} finally {
    $hashAlgorithm.Dispose()
}
$ruleName = "EasyMouseConnect-$suffix"
Write-Host "Rule name: $ruleName"
Write-Host "Inbound TCP $Port; program $resolvedProgram; peer $normalizedIp; Domain/Private profiles only."

if ($PSCmdlet.ShouldProcess($ruleName, 'Allow the specified peer to connect to Easy Mouse Connect')) {
    $existing = @(NetSecurity\Get-NetFirewallRule -Name $ruleName -ErrorAction SilentlyContinue)
    if ($existing.Count -gt 0) {
        $ports = @($existing | NetSecurity\Get-NetFirewallPortFilter)
        $apps = @($existing | NetSecurity\Get-NetFirewallApplicationFilter)
        $addresses = @($existing | NetSecurity\Get-NetFirewallAddressFilter)
        if ($existing.Count -ne 1 -or $ports.Count -ne 1 -or $apps.Count -ne 1 -or $addresses.Count -ne 1 -or
            $existing[0].Direction -ne 'Inbound' -or $existing[0].Action -ne 'Allow' -or
            $existing[0].Enabled -ne 'True' -or [int]$existing[0].Profile -ne 3 -or
            $ports[0].Protocol -notin @('TCP', '6') -or @($ports[0].LocalPort).Count -ne 1 -or
            [string]$ports[0].LocalPort -ne [string]$Port -or $apps[0].Program -ine $resolvedProgram -or
            @($addresses[0].RemoteAddress).Count -ne 1 -or [string]$addresses[0].RemoteAddress -ine $normalizedIp) {
            throw 'A rule with this name has different settings. Inspect it in Windows Firewall; this helper will not replace or broaden it.'
        }
        Write-Host 'The exact peer-scoped rule already exists and is enabled.'
        return
    }
    NetSecurity\New-NetFirewallRule -Name $ruleName -DisplayName "Easy Mouse Connect peer $normalizedIp" `
        -Direction Inbound -Action Allow -Protocol TCP -LocalPort $Port `
        -Program $resolvedProgram -RemoteAddress $normalizedIp -Profile Domain,Private | Out-Null
    Write-Host "Created. Remove later with: Remove-NetFirewallRule -Name '$ruleName'"
}

