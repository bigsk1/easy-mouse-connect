# Connecting two PCs

Easy Mouse Connect opens TCP port **24859** and connects to the IP address entered in the app. Both PCs run the app; the peer must be reachable and permitted by host firewall and network policy. The port carries separate authenticated input/control and bulk connections.

## LANs and routed subnets

Matching IP prefixes are not a requirement. `192.168.10.123` and `192.168.50.123` can communicate when a router knows both subnets and permits traffic between them. Guest Wi-Fi isolation, VLAN ACLs, duplicate VPN routes and Windows Firewall can prevent that connection even if both PCs have internet access.

Two private IPs at unrelated homes are usually unreachable from each other. The app does not create a VPN or perform NAT traversal. Use an existing route such as Tailscale for that case.

Check the receiver from Windows PowerShell on the initiating PC, replacing the documentation address with the receiver's actual address:

```powershell
Test-NetConnection -ComputerName 192.168.50.123 -Port 24859
```

Run this while the receiving app is open. `TcpTestSucceeded: True` establishes TCP reachability; it does not prove authentication or input sharing. The app rejects unauthenticated traffic. Ping failure alone is inconclusive because ICMP may be blocked.

The address parser supports literal IPv4 and IPv6 endpoints. When entering an explicit port with IPv6, use brackets, for example `[fd7a:115c:a1e0::1234]:24859`. IPv6 connection/listener behavior still belongs in the two-PC acceptance run; a parsing test does not validate a host's IPv6 routes or firewall.

## Tailscale

Install Tailscale separately, sign both PCs into an authorized tailnet, and allow communication between them in tailnet policy. Enter the other PC's Tailscale IP instead of its private LAN IP. The example addresses `100.100.94.12` and `100.71.9.14` are placeholders; do not use them as destinations unless they are actually yours.

```powershell
tailscale status
tailscale ping <peer-tailscale-ip>
Test-NetConnection -ComputerName <peer-tailscale-ip> -Port 24859
```

Tailscale can provide a direct path or an encrypted relay path when direct connectivity fails. The relay path may have higher latency, so connection success alone does not establish the same feel as a wired LAN. Inspect `tailscale status` to distinguish the path. See [Tailscale connection types](https://tailscale.com/docs/reference/connection-types).

Application encryption remains enabled inside the Tailscale tunnel. Tailnet membership is not a substitute for the app's pairing code and persistent peer authentication.

## Windows Firewall

The application does not create firewall exceptions. If a rule is needed, run the explicit helper from an **Administrator PowerShell** terminal. Supply the installed executable's full path and exactly one authorized peer IP:

```powershell
.\scripts\allow-peer-firewall.ps1 `
  -ProgramPath 'C:\Path\To\easy-mouse-connect.exe' `
  -PeerAddress '100.71.9.14' `
  -WhatIf
```

Review the displayed rule, then repeat without `-WhatIf` to apply it. The helper restricts inbound traffic to the specified executable, TCP port, single peer address and **Domain/Private** profiles. It rejects wildcard and subnet input. Create an equivalent narrow rule on the other PC if it must receive inbound connections.

Tailscale interfaces can have a different Windows network profile depending on the system. Check `Get-NetConnectionProfile`; the helper intentionally does not open Public-profile access. Review profile and firewall policy with the machine's administrator if this prevents a legitimate connection. Do not broadly disable Windows Firewall to test the app.

The helper prints a stable rule name. To remove a rule later, run:

```powershell
Remove-NetFirewallRule -Name '<name-printed-by-helper>'
```

Changing the executable path or peer IP can require replacing the rule. The current preview is intended for reachable LAN/VPN peers; router internet port forwarding is outside its supported setup.

## Diagnosing a failed connection

| Symptom | Next check |
| --- | --- |
| TCP check fails | App open on receiver, correct IP/port, route, network profile and scoped firewall rule |
| TCP works but pairing fails | Fresh 12-character code from the destination PC, correct address, five-minute expiry and remaining attempt budget |
| Trusted reconnect fails | Both sides still trust the same identities; restore the correct profile or revoke/re-pair deliberately |
| Works locally, fails over Tailscale | Tailnet policy, peer online state, tailnet destination address and Windows profile |
| Connects but pointer stutters | Wired LAN baseline, Tailscale direct/relay path, packet loss, hook thread load and queue pressure |
| Normal apps work but elevated app does not | Windows integrity/UIPI boundary; this version does not control elevated or secure desktops |
| Local controls appear captured | Press **Ctrl+Alt+Escape** on the controlling PC, then disconnect in the app |

