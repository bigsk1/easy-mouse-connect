//! Fixed-purpose desktop actions. No shell, executable, path or URL authority
//! is delegated to the WebView; only one normalized peer address is accepted.
use std::net::{IpAddr, SocketAddr};

pub const PROJECT_URL: &str = "https://github.com/bigsk1/easy-mouse-connect";
const FIREWALL_HELPER: &str = include_str!("../../../scripts/allow-peer-firewall.ps1");

pub fn peer_ip(address: &str) -> Result<IpAddr, String> {
    let address = address.trim();
    let ip = address
        .parse::<IpAddr>()
        .or_else(|_| address.parse::<SocketAddr>().map(|endpoint| endpoint.ip()))
        .map_err(|_| "Enter one literal peer IPv4 or IPv6 address.".to_string())?
        .to_canonical();
    if ip.is_unspecified() || ip.is_multicast() || matches!(ip, IpAddr::V4(ip) if ip.is_broadcast())
    {
        return Err(
            "Enter one unicast peer IP address; subnets and wildcards are not allowed.".into(),
        );
    }
    Ok(ip)
}

fn powershell_literal(text: &str) -> Result<String, String> {
    if text.contains(['\0', '\r', '\n']) {
        return Err("The application path is not supported by the firewall helper.".into());
    }
    Ok(format!("'{}'", text.replace('\'', "''")))
}

fn firewall_script(program: &str, ip: IpAddr) -> Result<String, String> {
    let program = powershell_literal(program)?;
    // All executable code is compiled into the application. Address text has
    // already become an IpAddr; neither input can introduce PowerShell syntax.
    Ok(format!(
        "$env:PSModulePath = \"$PSHOME\\Modules\"\n$ErrorActionPreference = 'Stop'\ntry {{\n& {{\n{FIREWALL_HELPER}\n}} -ProgramPath {program} -PeerAddress '{ip}' -Port 24859\nexit 0\n}} catch {{\nWrite-Error 'Easy Mouse Connect could not add the scoped firewall rule.' -ErrorAction Continue\nexit 1\n}}"
    ))
}

#[cfg(windows)]
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

#[cfg(windows)]
fn system_directory() -> Result<std::path::PathBuf, String> {
    use windows_sys::Win32::System::SystemInformation::GetSystemDirectoryW;
    let mut directory = [0u16; 32768];
    let length = unsafe { GetSystemDirectoryW(directory.as_mut_ptr(), directory.len() as u32) };
    if length == 0 || length as usize >= directory.len() {
        return Err("The Windows system directory is unavailable.".into());
    }
    Ok(std::path::PathBuf::from(String::from_utf16_lossy(
        &directory[..length as usize],
    )))
}

pub fn open_project() -> Result<(), String> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::{Shell::ShellExecuteW, WindowsAndMessaging::SW_SHOWNORMAL};
        let result = unsafe {
            ShellExecuteW(
                std::ptr::null_mut(),
                wide("open").as_ptr(),
                wide(PROJECT_URL).as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                SW_SHOWNORMAL,
            )
        };
        if result as usize <= 32 {
            return Err("Unable to open the project in your default browser.".into());
        }
        Ok(())
    }
    #[cfg(not(windows))]
    Err("Open the project link from the browser preview or Windows desktop app.".into())
}

pub fn add_firewall_rule(ip: IpAddr) -> Result<String, String> {
    #[cfg(windows)]
    {
        use base64::{engine::general_purpose::STANDARD, Engine as _};
        use windows_sys::Win32::{
            Foundation::{CloseHandle, GetLastError, ERROR_CANCELLED, WAIT_OBJECT_0},
            System::Threading::{GetExitCodeProcess, WaitForSingleObject, INFINITE},
            UI::{
                Shell::{
                    ShellExecuteExW, SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW,
                },
                WindowsAndMessaging::SW_SHOWNORMAL,
            },
        };
        let program = std::env::current_exe()
            .and_then(std::fs::canonicalize)
            .map_err(|_| "Unable to identify this application's executable.".to_string())?;
        let program = program
            .to_str()
            .ok_or_else(|| "The application path is not supported.".to_string())?;
        // Resolve-Path and the firewall API accept ordinary Windows paths. A
        // canonical current_exe can carry the extended-length prefix.
        let program = program.strip_prefix("\\\\?\\").unwrap_or(program);
        let encoded: Vec<u8> = firewall_script(program, ip)?
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        let parameters = wide(&format!(
            "-NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -EncodedCommand {}",
            STANDARD.encode(encoded)
        ));
        let executable = wide(
            system_directory()?
                .join("WindowsPowerShell/v1.0/powershell.exe")
                .to_str()
                .ok_or_else(|| "Windows PowerShell is unavailable.".to_string())?,
        );
        let verb = wide("runas");
        let mut info = SHELLEXECUTEINFOW {
            cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
            fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC,
            lpVerb: verb.as_ptr(),
            lpFile: executable.as_ptr(),
            lpParameters: parameters.as_ptr(),
            nShow: SW_SHOWNORMAL,
            ..unsafe { std::mem::zeroed() }
        };
        if unsafe { ShellExecuteExW(&mut info) } == 0 {
            return Err(if unsafe { GetLastError() } == ERROR_CANCELLED {
                "Windows approval was cancelled; no firewall rule was added.".into()
            } else {
                "Unable to start the Windows firewall helper.".into()
            });
        }
        if info.hProcess.is_null() {
            return Err("The firewall helper did not return a process handle.".into());
        }
        // This runs on a blocking worker. Keep its owned admission guard until
        // the real elevated process exits, even if its UI caller disappears.
        let finished = unsafe { WaitForSingleObject(info.hProcess, INFINITE) } == WAIT_OBJECT_0;
        let mut code = 1;
        let inspected = finished && unsafe { GetExitCodeProcess(info.hProcess, &mut code) } != 0;
        unsafe { CloseHandle(info.hProcess) };
        if !inspected {
            return Err("The firewall helper is still running or its result is unavailable. Inspect Windows Firewall before retrying.".into());
        }
        if code != 0 {
            return Err("The scoped firewall rule could not be added. Check administrator permissions and Windows Firewall policy.".into());
        }
        Ok("The peer-scoped inbound TCP 24859 rule is ready for this app on Private/Domain networks.".into())
    }
    #[cfg(not(windows))]
    {
        let _ = ip;
        Err("Firewall setup is available only in the Windows desktop app.".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn firewall_scope_accepts_one_host_and_never_subnets_or_code() {
        for value in [
            "192.0.2.10",
            "198.51.100.2:24859",
            "2001:db8::10",
            "[2001:db8::10]:24859",
        ] {
            assert!(peer_ip(value).is_ok(), "{value}");
        }
        for value in [
            "*",
            "0.0.0.0",
            "::",
            "255.255.255.255",
            "224.0.0.1",
            "ff02::1",
            "::ffff:0.0.0.0",
            "::ffff:255.255.255.255",
            "::ffff:224.0.0.1",
            "192.0.2.0/24",
            "host.example",
            "192.0.2.1';Start-Process cmd;#",
        ] {
            assert!(peer_ip(value).is_err(), "{value}");
        }
    }

    #[test]
    fn firewall_script_quotes_paths_and_has_fixed_authority() {
        let script = firewall_script(
            "C:\\Users\\O'Neil\\easy-mouse-connect.exe",
            "192.0.2.10".parse().unwrap(),
        )
        .unwrap();
        assert!(script.contains("-ProgramPath 'C:\\Users\\O''Neil\\easy-mouse-connect.exe'"));
        assert!(script.contains("-PeerAddress '192.0.2.10' -Port 24859"));
        assert!(script.contains("-RemoteAddress $normalizedIp -Profile Domain,Private"));
        assert!(!script.contains("-Profile Any"));
        assert!(powershell_literal("bad\npath").is_err());
    }
}
