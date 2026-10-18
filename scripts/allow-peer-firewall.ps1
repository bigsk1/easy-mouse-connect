[CmdletBinding(SupportsShouldProcess = $true, ConfirmImpact = 'Medium')]
param(
    [Parameter(Mandatory = $true)][string] $ProgramPath,
    [Parameter(Mandatory = $true)][string] $PeerAddress,
    [ValidateRange(1024, 65535)][int] $Port = 24859
)

. (Join-Path $PSScriptRoot 'common.ps1')
Assert-Windows

$peerIp = $null
if (-not [System.Net.IPAddress]::TryParse($PeerAddress, [ref] $peerIp)) {
    throw 'PeerAddress must be one literal IPv4 or IPv6 address. Wildcards, DNS names and subnets are not accepted.'
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
    if (Get-NetFirewallRule -Name $ruleName -ErrorAction SilentlyContinue) {
        Write-Host 'This exact rule already exists. Inspect it with Get-NetFirewallRule before changing policy.'
        return
    }
    New-NetFirewallRule -Name $ruleName -DisplayName "Easy Mouse Connect peer $normalizedIp" `
        -Direction Inbound -Action Allow -Protocol TCP -LocalPort $Port `
        -Program $resolvedProgram -RemoteAddress $normalizedIp -Profile Domain,Private | Out-Null
    Write-Host "Created. Remove later with: Remove-NetFirewallRule -Name '$ruleName'"
}

