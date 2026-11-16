//! What a machine says of itself, for checkd's own checks of the machine
//! they run on (`checkd:disk`, `checkd:memory`, `checkd:load`,
//! `checkd:cpu`, `checkd:service`) and for the inventory an agent sends
//! its engine: the system, its disks, its services and the ports it
//! listens on, from which nuthatch proposes checks.
//!
//! Everything here only reads. Linux answers from `/proc`, `statvfs` and
//! `systemctl`; Windows from its own calls, and the inventory from one
//! fixed PowerShell script.

use std::time::Duration;

/// A filesystem's room: all of it, what is used, and what an ordinary
/// user may still take.
#[derive(Debug, Clone, Copy)]
pub struct Space {
    pub total: u64,
    pub used: u64,
    pub free: u64,
}

impl Space {
    /// The share used, as `df` gives it: of what is used and what may
    /// still be taken, not counting what only root may take.
    pub fn used_pct(&self) -> f64 {
        let room = self.used + self.free;
        if room == 0 { 0.0 } else { self.used as f64 * 100.0 / room as f64 }
    }
}

#[cfg(unix)]
pub fn space(path: &str) -> Result<Space, String> {
    let c = std::ffi::CString::new(path).map_err(|_| format!("{path}: not a path"))?;
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c.as_ptr(), &mut stat) } != 0 {
        return Err(format!("{path}: {}", plain(&std::io::Error::last_os_error())));
    }
    let unit = stat.f_frsize as u64;
    let total = stat.f_blocks as u64 * unit;
    Ok(Space { total, used: total.saturating_sub(stat.f_bfree as u64 * unit), free: stat.f_bavail as u64 * unit })
}

#[cfg(windows)]
pub fn space(path: &str) -> Result<Space, String> {
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    // `C:` is the drive's current directory to Windows; `C:\` the drive.
    let mut root = path.replace('/', "\\");
    if !root.ends_with('\\') {
        root.push('\\');
    }
    let wide: Vec<u16> = root.encode_utf16().chain(Some(0)).collect();
    let (mut free, mut total, mut total_free) = (0u64, 0u64, 0u64);
    if unsafe { GetDiskFreeSpaceExW(wide.as_ptr(), &mut free, &mut total, &mut total_free) } == 0 {
        return Err(format!("{path}: {}", plain(&std::io::Error::last_os_error())));
    }
    Ok(Space { total, used: total.saturating_sub(total_free), free })
}

/// Memory: all of it and what is available without swapping.
#[cfg(unix)]
pub fn memory() -> Result<(u64, u64), String> {
    let text = std::fs::read_to_string("/proc/meminfo").map_err(|err| format!("/proc/meminfo: {}", plain(&err)))?;
    let field = |name: &str| {
        text.lines()
            .find_map(|line| line.strip_prefix(name)?.strip_prefix(':'))
            .and_then(|rest| rest.split_whitespace().next()?.parse::<u64>().ok())
            .map(|kib| kib * 1024)
    };
    match (field("MemTotal"), field("MemAvailable")) {
        (Some(total), Some(available)) => Ok((total, available)),
        _ => Err("/proc/meminfo has no MemTotal or MemAvailable".to_string()),
    }
}

#[cfg(windows)]
pub fn memory() -> Result<(u64, u64), String> {
    use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    let mut status: MEMORYSTATUSEX = unsafe { std::mem::zeroed() };
    status.dwLength = std::mem::size_of::<MEMORYSTATUSEX>() as u32;
    if unsafe { GlobalMemoryStatusEx(&mut status) } == 0 {
        return Err(plain(&std::io::Error::last_os_error()));
    }
    Ok((status.ullTotalPhys, status.ullAvailPhys))
}

/// The load average over 1, 5 and 15 minutes; Windows has none.
#[cfg(unix)]
pub fn load() -> Option<[f64; 3]> {
    let text = std::fs::read_to_string("/proc/loadavg").ok()?;
    let mut numbers = text.split_whitespace().map(|n| n.parse::<f64>().ok());
    Some([numbers.next()??, numbers.next()??, numbers.next()??])
}

#[cfg(windows)]
pub fn load() -> Option<[f64; 3]> {
    None
}

pub fn cpus() -> usize {
    std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1)
}

/// Processor time so far: (busy, all), in the system's own units.
#[cfg(unix)]
fn cpu_times() -> Result<(u64, u64), String> {
    let text = std::fs::read_to_string("/proc/stat").map_err(|err| format!("/proc/stat: {}", plain(&err)))?;
    let line = text.lines().find(|l| l.starts_with("cpu ")).ok_or("/proc/stat has no cpu line")?;
    let n: Vec<u64> = line.split_whitespace().skip(1).filter_map(|v| v.parse().ok()).collect();
    if n.len() < 4 {
        return Err("/proc/stat: a short cpu line".to_string());
    }
    // user nice system idle iowait irq softirq steal; guest time is in user.
    let all: u64 = n.iter().take(8).sum();
    let idle = n[3] + n.get(4).copied().unwrap_or(0);
    Ok((all - idle, all))
}

#[cfg(windows)]
fn cpu_times() -> Result<(u64, u64), String> {
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::System::Threading::GetSystemTimes;
    let zero = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
    let (mut idle, mut kernel, mut user) = (zero, zero, zero);
    if unsafe { GetSystemTimes(&mut idle, &mut kernel, &mut user) } == 0 {
        return Err(plain(&std::io::Error::last_os_error()));
    }
    let n = |t: FILETIME| ((t.dwHighDateTime as u64) << 32) | t.dwLowDateTime as u64;
    // Kernel time includes the idle time.
    let all = n(kernel) + n(user);
    Ok((all - n(idle), all))
}

/// The share of processor time busy over the next second.
pub async fn cpu_busy() -> Result<f64, String> {
    let (busy0, all0) = cpu_times()?;
    tokio::time::sleep(Duration::from_secs(1)).await;
    let (busy1, all1) = cpu_times()?;
    let all = all1.saturating_sub(all0);
    Ok(if all == 0 { 0.0 } else { busy1.saturating_sub(busy0) as f64 * 100.0 / all as f64 })
}

/// A service's name as the system takes it, and nothing a command could
/// read as an option.
pub fn service_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 256
        && !name.starts_with('-')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '@' | ':' | '$' | ' '))
}

/// Whether a service runs, and its state in the system's words. `None`:
/// there is no such service.
#[cfg(unix)]
pub async fn service(name: &str) -> Result<Option<(bool, String)>, String> {
    if !service_name(name) {
        return Err(format!("'{name}' is not a service's name"));
    }
    let output = tokio::process::Command::new("systemctl")
        .args(["show", "--property=LoadState,ActiveState,SubState", "--", name])
        .kill_on_drop(true)
        .output()
        .await
        .map_err(|err| format!("systemctl: {}", plain(&err)))?;
    let text = String::from_utf8_lossy(&output.stdout);
    let field = |key: &str| text.lines().find_map(|l| l.strip_prefix(key)?.strip_prefix('=')).unwrap_or("").to_string();
    if !output.status.success() {
        return Err(format!("systemctl: {}", String::from_utf8_lossy(&output.stderr).trim()));
    }
    if field("LoadState") == "not-found" {
        return Ok(None);
    }
    let (active, sub) = (field("ActiveState"), field("SubState"));
    Ok(Some((active == "active", format!("{active} ({sub})"))))
}

#[cfg(windows)]
pub async fn service(name: &str) -> Result<Option<(bool, String)>, String> {
    use windows_sys::Win32::System::Services::*;
    if !service_name(name) {
        return Err(format!("'{name}' is not a service's name"));
    }
    let wide: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
    unsafe {
        let manager = OpenSCManagerW(std::ptr::null(), std::ptr::null(), SC_MANAGER_CONNECT);
        if manager.is_null() {
            return Err(format!("the service manager: {}", plain(&std::io::Error::last_os_error())));
        }
        let service = OpenServiceW(manager, wide.as_ptr(), SERVICE_QUERY_STATUS);
        if service.is_null() {
            let err = std::io::Error::last_os_error();
            CloseServiceHandle(manager);
            // ERROR_SERVICE_DOES_NOT_EXIST
            return if err.raw_os_error() == Some(1060) { Ok(None) } else { Err(plain(&err)) };
        }
        let mut status: SERVICE_STATUS = std::mem::zeroed();
        let asked = QueryServiceStatus(service, &mut status);
        let err = std::io::Error::last_os_error();
        CloseServiceHandle(service);
        CloseServiceHandle(manager);
        if asked == 0 {
            return Err(plain(&err));
        }
        let state = match status.dwCurrentState {
            SERVICE_STOPPED => "stopped",
            SERVICE_START_PENDING => "starting",
            SERVICE_STOP_PENDING => "stopping",
            SERVICE_RUNNING => "running",
            SERVICE_CONTINUE_PENDING => "continuing",
            SERVICE_PAUSE_PENDING => "pausing",
            SERVICE_PAUSED => "paused",
            _ => "in an unknown state",
        };
        Ok(Some((status.dwCurrentState == SERVICE_RUNNING, state.to_string())))
    }
}

fn plain(err: &std::io::Error) -> String {
    let text = err.to_string();
    match text.find(" (os error") {
        Some(at) => text[..at].to_string(),
        None => text,
    }
}

/// A text as a JSON string.
fn json(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Filesystems that hold data, as against those of the kernel, of memory
/// or of images that only read.
#[cfg(unix)]
const REAL: [&str; 12] = ["ext2", "ext3", "ext4", "xfs", "btrfs", "zfs", "f2fs", "vfat", "exfat", "ntfs", "ntfs3", "jfs"];

/// The mounts with data on them: each device once, at its shortest path.
#[cfg(unix)]
fn mounts() -> Vec<(String, String)> {
    let text = std::fs::read_to_string("/proc/self/mounts").unwrap_or_default();
    let mut seen: Vec<(String, String, String)> = Vec::new();
    for line in text.lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() < 3 || !REAL.contains(&f[2]) {
            continue;
        }
        // The kernel writes a space in a path as \040.
        let path = f[1].replace("\\040", " ");
        if path.starts_with("/snap/") || path.starts_with("/var/lib/docker/") || path.starts_with("/var/lib/containers/") {
            continue;
        }
        match seen.iter_mut().find(|(device, _, _)| device == f[0]) {
            Some(known) if path.len() < known.1.len() => known.1 = path,
            Some(_) => {}
            None => seen.push((f[0].to_string(), path, f[2].to_string())),
        }
    }
    let mut out: Vec<(String, String)> = seen.into_iter().map(|(_, path, fs)| (path, fs)).collect();
    out.sort();
    out
}

/// The TCP ports listened on, with the address: `0.0.0.0`, `::` or one.
#[cfg(unix)]
fn listening() -> Vec<(u16, String)> {
    let mut out: Vec<(u16, String)> = Vec::new();
    for (file, v6) in [("/proc/net/tcp", false), ("/proc/net/tcp6", true)] {
        let Ok(text) = std::fs::read_to_string(file) else { continue };
        for line in text.lines().skip(1) {
            let f: Vec<&str> = line.split_whitespace().collect();
            // st 0A is LISTEN.
            if f.len() < 4 || f[3] != "0A" {
                continue;
            }
            let Some((address, port)) = f[1].split_once(':') else { continue };
            let Ok(port) = u16::from_str_radix(port, 16) else { continue };
            let address = proc_address(address, v6);
            if !out.iter().any(|(p, a)| *p == port && *a == address) {
                out.push((port, address));
            }
        }
    }
    out.sort();
    out
}

/// An address of `/proc/net/tcp`: the hex of its bytes, a word at a time
/// in the machine's order.
#[cfg(unix)]
fn proc_address(hex: &str, v6: bool) -> String {
    let words: Vec<u32> = (0..hex.len() / 8).filter_map(|i| u32::from_str_radix(&hex[i * 8..i * 8 + 8], 16).ok()).collect();
    let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_ne_bytes()).collect();
    if !v6 && bytes.len() == 4 {
        std::net::Ipv4Addr::new(bytes[0], bytes[1], bytes[2], bytes[3]).to_string()
    } else if bytes.len() == 16 {
        let mut a = [0u8; 16];
        a.copy_from_slice(&bytes);
        std::net::Ipv6Addr::from(a).to_string()
    } else {
        hex.to_string()
    }
}

#[cfg(unix)]
fn os_release() -> (String, String, String) {
    let text = std::fs::read_to_string("/etc/os-release").or_else(|_| std::fs::read_to_string("/usr/lib/os-release")).unwrap_or_default();
    let field = |key: &str| {
        text.lines()
            .find_map(|l| l.strip_prefix(key)?.strip_prefix('='))
            .map(|v| v.trim_matches('"').to_string())
            .unwrap_or_default()
    };
    let name = Some(field("PRETTY_NAME")).filter(|n| !n.is_empty()).unwrap_or_else(|| field("NAME"));
    (name, field("ID"), field("VERSION_ID"))
}

/// The services running, with what they say they are.
#[cfg(unix)]
async fn running_services() -> Vec<(String, String)> {
    let Ok(output) = tokio::process::Command::new("systemctl")
        .args(["list-units", "--type=service", "--state=running", "--no-legend", "--plain", "--no-pager"])
        .kill_on_drop(true)
        .output()
        .await
    else {
        return Vec::new();
    };
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            // UNIT LOAD ACTIVE SUB DESCRIPTION...
            let mut f = line.split_whitespace();
            let unit = f.next()?.to_string();
            let description = f.skip(3).collect::<Vec<_>>().join(" ");
            Some((unit, description))
        })
        .collect()
}

/// What this machine is and runs, as one JSON object, with the version
/// of the program that says it (`agent`), for knowing which agents are
/// older than their engine.
pub async fn inventory() -> String {
    with_agent(&machine().await)
}

fn with_agent(machine: &str) -> String {
    match machine.strip_prefix('{') {
        Some(rest) => format!(r#"{{"agent":{},{rest}"#, json(env!("CHECKD_VERSION"))),
        None => machine.to_string(),
    }
}

#[cfg(unix)]
async fn machine() -> String {
    let (name, id, version) = os_release();
    let read = |path: &str| std::fs::read_to_string(path).map(|t| t.trim().to_string()).unwrap_or_default();
    let (memory, _) = memory().unwrap_or((0, 0));
    let disks: Vec<String> = mounts()
        .into_iter()
        .filter_map(|(path, fs)| {
            let space = space(&path).ok()?;
            Some(format!(
                r#"{{"path":{},"fs":{},"size":{},"used_pct":{:.1}}}"#,
                json(&path),
                json(&fs),
                space.total,
                space.used_pct()
            ))
        })
        .collect();
    let services: Vec<String> = running_services()
        .await
        .into_iter()
        .map(|(unit, description)| format!(r#"{{"name":{},"description":{}}}"#, json(&unit), json(&description)))
        .collect();
    let ports: Vec<String> = listening().into_iter().map(|(port, address)| format!(r#"{{"port":{port},"address":{}}}"#, json(&address))).collect();
    format!(
        r#"{{"version":1,"os":{{"family":"linux","name":{},"id":{},"version":{},"kernel":{},"arch":{}}},"hostname":{},"cpus":{},"memory":{},"disks":[{}],"services":[{}],"ports":[{}]}}"#,
        json(&name),
        json(&id),
        json(&version),
        json(&read("/proc/sys/kernel/osrelease")),
        json(std::env::consts::ARCH),
        json(&read("/proc/sys/kernel/hostname")),
        cpus(),
        memory,
        disks.join(","),
        services.join(","),
        ports.join(",")
    )
}

/// The same from Windows, by one fixed script: what it asks is read only.
#[cfg(windows)]
const WINDOWS_INVENTORY: &str = r#"
$ErrorActionPreference = 'SilentlyContinue'
$os = Get-CimInstance Win32_OperatingSystem
$disks = @(Get-CimInstance Win32_LogicalDisk -Filter 'DriveType=3' | ForEach-Object {
  $used = 0; if ($_.Size -gt 0) { $used = [math]::Round(($_.Size - $_.FreeSpace) * 100 / $_.Size, 1) }
  [ordered]@{ path = $_.DeviceID; fs = $_.FileSystem; size = [int64]$_.Size; used_pct = $used } })
$services = @(Get-Service | Where-Object Status -eq 'Running' | ForEach-Object { [ordered]@{ name = $_.Name; description = $_.DisplayName } })
$ports = @(Get-NetTCPConnection -State Listen | Sort-Object LocalPort, LocalAddress -Unique | ForEach-Object { [ordered]@{ port = [int]$_.LocalPort; address = $_.LocalAddress } })
[ordered]@{
  version = 1
  os = [ordered]@{ family = 'windows'; name = $os.Caption; id = 'windows'; version = $os.Version; kernel = $os.BuildNumber; arch = $os.OSArchitecture }
  hostname = $env:COMPUTERNAME
  cpus = [Environment]::ProcessorCount
  memory = [int64]$os.TotalVisibleMemorySize * 1024
  disks = $disks; services = $services; ports = $ports
} | ConvertTo-Json -Depth 4 -Compress
"#;

#[cfg(windows)]
async fn machine() -> String {
    let fallback = || {
        let (memory, _) = memory().unwrap_or((0, 0));
        format!(
            r#"{{"version":1,"os":{{"family":"windows","name":"Windows","id":"windows","version":"","kernel":"","arch":{}}},"hostname":"","cpus":{},"memory":{memory},"disks":[],"services":[],"ports":[]}}"#,
            json(std::env::consts::ARCH),
            cpus()
        )
    };
    let run = tokio::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", WINDOWS_INVENTORY])
        .kill_on_drop(true)
        .output();
    match tokio::time::timeout(Duration::from_secs(60), run).await {
        Ok(Ok(output)) if output.status.success() => {
            let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if text.starts_with('{') { text } else { fallback() }
        }
        _ => fallback(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_a_service_may_have() {
        assert!(service_name("nginx.service") && service_name("getty@tty1.service") && service_name("MSSQL$SQLEXPRESS"));
        assert!(!service_name("--now") && !service_name("a;b") && !service_name(""));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn this_machine() {
        let (total, available) = memory().unwrap();
        assert!(total > 0 && available <= total);
        assert!(load().is_some());
        let root = space("/").unwrap();
        assert!(root.total > 0 && root.used_pct() <= 100.0);
        assert!(space("/no/such/place").is_err());
        assert_eq!(proc_address("0100007F", false), "127.0.0.1");
        assert_eq!(proc_address("00000000", false), "0.0.0.0");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn an_inventory_is_json() {
        let text = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(inventory());
        assert!(text.starts_with(&format!(r#"{{"agent":"{}","version":1,"os":{{"family":"linux""#, env!("CHECKD_VERSION"))), "{text}");
        assert!(text.contains(r#""disks":[{"path":"#), "{text}");
    }
}
