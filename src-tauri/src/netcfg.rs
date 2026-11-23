//! This PC's network adapters: their IPv4 addresses (for the TFTP panel) and
//! switching an adapter between DHCP and a static address, so the PC can sit on
//! a board's network (192.168.1.x and so on) and go back afterwards.
//!
//! Changing an adapter needs administrator rights. Windows: `netsh`, started
//! through a UAC prompt for each change; the app itself never runs elevated.
//! Linux: NetworkManager's `nmcli`, which asks polkit when it has to.

use std::net::Ipv4Addr;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IfAddress {
    pub ip: String,
    pub prefix: u8,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Adapter {
    /// What the set commands take: the interface index on Windows, the name on Linux.
    pub id: String,
    /// "Ethernet 2", "enp3s0".
    pub name: String,
    /// "Realtek PCIe 2.5GbE Family Controller" (Windows only).
    pub description: String,
    /// `dhcp`, `manual`, and on Linux also `link-local`, `shared`, `disabled`;
    /// `none` when NetworkManager has no connection for it, `unknown` without nmcli.
    pub method: String,
    /// Address from DHCP; None when unknown.
    pub dhcp: Option<bool>,
    /// A cable / link is there.
    pub connected: bool,
    pub addresses: Vec<IfAddress>,
    pub gateways: Vec<String>,
    pub wireless: bool,
}

/// How an adapter was set up, to put it back exactly.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Saved {
    /// As in [`Adapter::method`].
    pub method: String,
    /// Configured addresses, "ip/prefix".
    pub addresses: Vec<String>,
    pub gateway: Option<String>,
    /// Linux: the NetworkManager connection; None when there was none.
    pub connection: Option<String>,
    /// Linux: ipv4.never-default (no default route through it).
    #[serde(default)]
    pub never_default: Option<bool>,
    /// Linux: ipv4.ignore-auto-dns.
    #[serde(default)]
    pub ignore_auto_dns: Option<bool>,
    /// Windows: the interface metric, "auto" or a number.
    #[serde(default)]
    pub metric: Option<String>,
}

/// Windows interface metric for a board link: far behind any real uplink.
#[cfg_attr(not(windows), allow(dead_code))]
pub const BOARD_LINK_METRIC: u32 = 9000;

/// The adapter after a change, and how it was before.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Changed {
    pub adapter: Adapter,
    pub before: Saved,
}

/// Names of adapters that are not hardware: changing them breaks VPNs, VMs or containers.
#[cfg_attr(not(windows), allow(dead_code))]
fn looks_virtual(name: &str, description: &str) -> bool {
    const HINTS: [&str; 16] = [
        "virtual",
        "hyper-v",
        "vethernet",
        "vpn",
        "tap-",
        "tap ",
        "wan miniport",
        "bluetooth",
        "loopback",
        "pseudo",
        "wi-fi direct",
        "vmware",
        "virtualbox",
        "npcap",
        "tunnel",
        "wireguard",
    ];
    let text = format!("{name} {description}").to_lowercase();
    HINTS.iter().any(|h| text.contains(h))
}

pub fn mask_of(prefix: u8) -> Ipv4Addr {
    Ipv4Addr::from(if prefix == 0 { 0 } else { u32::MAX << (32 - prefix.min(32) as u32) })
}

/// Checks a static setting; the messages are ready to show.
pub fn validate(ip: &str, prefix: u8, gateway: Option<&str>) -> Result<(Ipv4Addr, Option<Ipv4Addr>), String> {
    let addr: Ipv4Addr = ip.trim().parse().map_err(|_| format!("Not an IPv4 address: {ip}"))?;
    if !(1..=30).contains(&prefix) {
        return Err("The prefix length must be between 1 and 30 (24 = 255.255.255.0)".into());
    }
    if addr.is_unspecified() || addr.is_loopback() || addr.is_multicast() || addr.is_broadcast() {
        return Err(format!("{addr} cannot be an adapter address"));
    }
    let mask = u32::from(mask_of(prefix));
    let host = u32::from(addr) & !mask;
    if host == 0 || host == !mask {
        return Err(format!("{addr}/{prefix} is a network or broadcast address; pick another host number"));
    }
    let gw = match gateway.map(str::trim).filter(|g| !g.is_empty()) {
        None => None,
        Some(g) => {
            let g: Ipv4Addr = g.parse().map_err(|_| format!("Not an IPv4 address: {g}"))?;
            if u32::from(g) & mask != u32::from(addr) & mask {
                return Err(format!("The gateway {g} is not on {addr}/{prefix}"));
            }
            Some(g)
        }
    };
    Ok((addr, gw))
}

// ------------------------------------------------------------------ Windows

#[cfg(windows)]
mod sys {
    use super::*;
    use windows_sys::Win32::NetworkManagement::IpHelper::{
        GetAdaptersAddresses, GAA_FLAG_INCLUDE_GATEWAYS, GAA_FLAG_SKIP_ANYCAST, GAA_FLAG_SKIP_DNS_SERVER,
        GAA_FLAG_SKIP_MULTICAST, IP_ADAPTER_ADDRESSES_LH,
    };
    use windows_sys::Win32::NetworkManagement::Ndis::IfOperStatusUp;
    use windows_sys::Win32::Networking::WinSock::{AF_INET, SOCKADDR, SOCKADDR_IN};

    const IP_ADAPTER_DHCP_ENABLED: u32 = 0x4;
    const IF_TYPE_SOFTWARE_LOOPBACK: u32 = 24;
    const IF_TYPE_IEEE80211: u32 = 71;
    const ERROR_BUFFER_OVERFLOW: u32 = 111;

    unsafe fn wide(p: *const u16) -> String {
        if p.is_null() {
            return String::new();
        }
        let mut len = 0;
        while *p.add(len) != 0 {
            len += 1;
        }
        String::from_utf16_lossy(std::slice::from_raw_parts(p, len))
    }

    unsafe fn v4(sa: *const SOCKADDR) -> Option<Ipv4Addr> {
        if sa.is_null() || (*sa).sa_family != AF_INET {
            return None;
        }
        let sin = &*(sa as *const SOCKADDR_IN);
        Some(Ipv4Addr::from(u32::from_be(sin.sin_addr.S_un.S_addr)))
    }

    const IF_TYPE_ETHERNET: u32 = 6;

    /// Hardware Ethernet and Wi-Fi adapters, with or without a cable (one can be set up
    /// before the board is plugged in). Virtual ones (VPN, Hyper-V, …) are left out.
    pub fn adapters() -> Vec<Adapter> {
        all()
            .into_iter()
            .filter(|(a, if_type)| {
                (a.connected || *if_type == IF_TYPE_ETHERNET || *if_type == IF_TYPE_IEEE80211)
                    && !looks_virtual(&a.name, &a.description)
            })
            .map(|(a, _)| a)
            .collect()
    }

    /// Every adapter but loopback, with its interface type.
    fn all() -> Vec<(Adapter, u32)> {
        let flags =
            GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST | GAA_FLAG_SKIP_DNS_SERVER | GAA_FLAG_INCLUDE_GATEWAYS;
        let mut size: u32 = 32 * 1024;
        for _ in 0..4 {
            // u64 elements: the structures need 8-byte alignment.
            let mut buf = vec![0u64; (size as usize).div_ceil(8)];
            let first = buf.as_mut_ptr() as *mut IP_ADAPTER_ADDRESSES_LH;
            let ret = unsafe { GetAdaptersAddresses(AF_INET as u32, flags, std::ptr::null(), first, &mut size) };
            if ret == ERROR_BUFFER_OVERFLOW {
                continue;
            }
            if ret != 0 {
                return Vec::new();
            }
            let mut out = Vec::new();
            let mut p = first as *const IP_ADAPTER_ADDRESSES_LH;
            // SAFETY: a linked list inside `buf`, filled in by GetAdaptersAddresses.
            unsafe {
                while !p.is_null() {
                    let a = &*p;
                    p = a.Next;
                    if a.IfType == IF_TYPE_SOFTWARE_LOOPBACK {
                        continue;
                    }
                    let mut addresses = Vec::new();
                    let mut u = a.FirstUnicastAddress;
                    while !u.is_null() {
                        if let Some(ip) = v4((*u).Address.lpSockaddr) {
                            addresses.push(IfAddress { ip: ip.to_string(), prefix: (*u).OnLinkPrefixLength });
                        }
                        u = (*u).Next;
                    }
                    let mut gateways = Vec::new();
                    let mut g = a.FirstGatewayAddress;
                    while !g.is_null() {
                        if let Some(ip) = v4((*g).Address.lpSockaddr) {
                            gateways.push(ip.to_string());
                        }
                        g = (*g).Next;
                    }
                    let dhcp = a.Anonymous2.Flags & IP_ADAPTER_DHCP_ENABLED != 0;
                    let connected = a.OperStatus == IfOperStatusUp;
                    // A disconnected adapter can still report its last DHCP address.
                    if !connected {
                        addresses.clear();
                        gateways.clear();
                    }
                    out.push((
                        Adapter {
                            id: a.Anonymous1.Anonymous.IfIndex.to_string(),
                            name: wide(a.FriendlyName),
                            description: wide(a.Description),
                            method: if dhcp { "dhcp" } else { "manual" }.into(),
                            dhcp: Some(dhcp),
                            connected,
                            addresses,
                            gateways,
                            wireless: a.IfType == IF_TYPE_IEEE80211,
                        },
                        a.IfType,
                    ));
                }
            }
            return out;
        }
        Vec::new()
    }

    /// Addresses of adapters that are up, virtual ones included (a VM's network can be
    /// where the device is).
    pub fn ipv4_addresses() -> Vec<(String, Ipv4Addr, u8)> {
        all()
            .into_iter()
            .map(|(a, _)| a)
            .filter(|a| a.connected)
            .flat_map(|a| {
                let name = a.name;
                a.addresses.into_iter().filter_map(move |x| Some((name.clone(), x.ip.parse().ok()?, x.prefix)))
            })
            .collect()
    }

    fn quote(s: &str) -> String {
        format!("\"{s}\"")
    }

    /// Runs `commands` (each a full command line) in one elevated `cmd`, after a UAC
    /// prompt. Returns their combined output, or why it did not run.
    fn run_elevated(commands: &[String]) -> Result<String, String> {
        use windows_sys::Win32::Foundation::{CloseHandle, GetLastError};
        use windows_sys::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject};
        use windows_sys::Win32::UI::Shell::{ShellExecuteExW, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW};

        const ERROR_CANCELLED: u32 = 1223;
        const SW_HIDE: i32 = 0;

        let out = std::env::temp_dir().join(format!("termworker-netcfg-{}.txt", std::process::id()));
        let _ = std::fs::remove_file(&out);
        // cmd /c "(a & b) > "file" 2>&1": cmd strips the outer quotes.
        let params = format!("/d /c \"({}) > {} 2>&1\"", commands.join(" & "), quote(&out.display().to_string()));
        let w = |s: &str| s.encode_utf16().chain(std::iter::once(0)).collect::<Vec<u16>>();
        let (verb, file, params) = (w("runas"), w("cmd.exe"), w(&params));

        let code = unsafe {
            let mut info: SHELLEXECUTEINFOW = std::mem::zeroed();
            info.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
            info.fMask = SEE_MASK_NOCLOSEPROCESS;
            info.lpVerb = verb.as_ptr();
            info.lpFile = file.as_ptr();
            info.lpParameters = params.as_ptr();
            info.nShow = SW_HIDE;
            if ShellExecuteExW(&mut info) == 0 {
                let e = GetLastError();
                return Err(if e == ERROR_CANCELLED {
                    "Cancelled: Windows did not get administrator permission".into()
                } else {
                    format!("Could not run netsh as administrator (error {e})")
                });
            }
            if info.hProcess.is_null() {
                return Err("Could not run netsh as administrator".into());
            }
            WaitForSingleObject(info.hProcess, 60_000);
            let mut code = 0u32;
            GetExitCodeProcess(info.hProcess, &mut code);
            CloseHandle(info.hProcess);
            code
        };
        let text = std::fs::read(&out).map(|b| decode_console(&b)).unwrap_or_default();
        let _ = std::fs::remove_file(&out);
        log::info!("netcfg: {commands:?} -> {code}: {}", text.trim());
        Ok(text)
    }

    /// netsh writes in the console code page (CP949 on Korean Windows).
    fn decode_console(b: &[u8]) -> String {
        match std::str::from_utf8(b) {
            Ok(s) => s.to_string(),
            Err(_) => encoding_rs::EUC_KR.decode(b).0.into_owned(),
        }
    }

    fn index(id: &str) -> Result<u32, String> {
        id.parse().map_err(|_| format!("Unknown adapter {id}"))
    }

    fn static_cmds(idx: u32, ip: Ipv4Addr, prefix: u8, gateway: Option<Ipv4Addr>) -> Vec<String> {
        let gw = gateway.map(|g| g.to_string()).unwrap_or_else(|| "none".into());
        vec![format!(
            "netsh interface ipv4 set address name={idx} source=static address={ip} mask={} gateway={gw}",
            mask_of(prefix)
        )]
    }

    fn dhcp_cmds(idx: u32) -> Vec<String> {
        vec![
            format!("netsh interface ipv4 set address name={idx} source=dhcp"),
            format!("netsh interface ipv4 set dnsservers name={idx} source=dhcp"),
        ]
    }

    /// "auto" (Windows picks by link speed) or a fixed number.
    fn metric_cmd(idx: u32, metric: &str) -> String {
        let arg = if metric == "auto" {
            "-AutomaticMetric Enabled".to_string()
        } else {
            format!("-InterfaceMetric {metric}")
        };
        format!(
            "powershell -NoProfile -NonInteractive -Command \"Set-NetIPInterface -InterfaceIndex {idx} -AddressFamily IPv4 {arg}\""
        )
    }

    /// The IPv4 interface metric now: "auto" or the fixed number.
    fn metric_of(idx: u32) -> Option<String> {
        use windows_sys::Win32::NetworkManagement::IpHelper::{
            GetIpInterfaceEntry, InitializeIpInterfaceEntry, MIB_IPINTERFACE_ROW,
        };
        unsafe {
            let mut row: MIB_IPINTERFACE_ROW = std::mem::zeroed();
            InitializeIpInterfaceEntry(&mut row);
            row.Family = AF_INET;
            row.InterfaceIndex = idx;
            if GetIpInterfaceEntry(&mut row) != 0 {
                return None;
            }
            Some(if row.UseAutomaticMetric { "auto".into() } else { row.Metric.to_string() })
        }
    }

    pub fn set_static(id: &str, ip: Ipv4Addr, prefix: u8, gateway: Option<Ipv4Addr>) -> Result<String, String> {
        run_elevated(&static_cmds(index(id)?, ip, prefix, gateway))
    }

    /// DHCP; a board link also gets a metric far behind the real uplink, so the board's
    /// default route and DNS are only used when nothing else is there.
    pub fn set_dhcp(id: &str, board_link: bool) -> Result<String, String> {
        let idx = index(id)?;
        let mut cmds = dhcp_cmds(idx);
        if board_link {
            cmds.push(metric_cmd(idx, &BOARD_LINK_METRIC.to_string()));
        }
        run_elevated(&cmds)
    }

    pub fn snapshot(id: &str) -> Result<Saved, String> {
        let a =
            all().into_iter().map(|(a, _)| a).find(|a| a.id == id).ok_or_else(|| format!("Unknown adapter {id}"))?;
        Ok(Saved {
            metric: metric_of(index(id)?),
            method: a.method,
            addresses: a.addresses.iter().map(|x| format!("{}/{}", x.ip, x.prefix)).collect(),
            gateway: a.gateways.first().cloned(),
            connection: None,
            never_default: None,
            ignore_auto_dns: None,
        })
    }

    /// Back to DHCP or the first static address (netsh sets one; extra ones are not
    /// restored), and the metric. One UAC prompt.
    pub fn restore(id: &str, saved: &Saved) -> Result<String, String> {
        let idx = index(id)?;
        let first = saved.addresses.first().and_then(|c| super::parse_cidr(c));
        let mut cmds = match (saved.method.as_str(), first) {
            ("manual", Some((ip, prefix))) => {
                static_cmds(idx, ip, prefix, saved.gateway.as_deref().and_then(|g| g.parse().ok()))
            }
            _ => dhcp_cmds(idx),
        };
        if let Some(m) = &saved.metric {
            cmds.push(metric_cmd(idx, m));
        }
        run_elevated(&cmds)
    }
}

// ------------------------------------------------------------------ Linux

#[cfg(unix)]
mod sys {
    use super::*;
    use std::process::Command;

    pub fn ipv4_addresses() -> Vec<(String, Ipv4Addr, u8)> {
        // Linux keeps the address of a link without carrier (an unplugged cable, a
        // docker bridge with nothing on it): list only links that are up (IFF_RUNNING).
        if_addrs::get_if_addrs()
            .unwrap_or_default()
            .into_iter()
            .filter(|i| i.is_oper_up())
            .filter_map(|i| match i.addr {
                if_addrs::IfAddr::V4(a) => Some((i.name, a.ip, a.prefixlen)),
                _ => None,
            })
            .collect()
    }

    fn nmcli(args: &[&str]) -> Result<String, String> {
        let out = Command::new("nmcli").args(args).output().map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                "Changing the address needs NetworkManager (nmcli), which is not installed".to_string()
            } else {
                format!("nmcli: {e}")
            }
        })?;
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
        } else {
            let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
            Err(if err.is_empty() { format!("nmcli failed ({})", out.status) } else { err })
        }
    }

    /// The NetworkManager connection active on `dev`, if any.
    fn connection(dev: &str) -> Option<String> {
        nmcli(&["-g", "GENERAL.CONNECTION", "device", "show", dev]).ok().filter(|c| !c.is_empty() && c != "--")
    }

    /// Default gateways per interface, from /proc/net/route.
    fn gateways() -> Vec<(String, Ipv4Addr)> {
        let text = std::fs::read_to_string("/proc/net/route").unwrap_or_default();
        text.lines()
            .skip(1)
            .filter_map(|l| {
                let f: Vec<&str> = l.split_whitespace().collect();
                if f.len() < 3 || f[1] != "00000000" {
                    return None;
                }
                let gw = u32::from_str_radix(f[2], 16).ok()?;
                Some((f[0].to_string(), Ipv4Addr::from(u32::from_be(gw))))
            })
            .collect()
    }

    /// Hardware (has /sys/class/net/X/device): not a bridge, veth, tun or docker network.
    fn is_hardware(dev: &str) -> bool {
        std::path::Path::new(&format!("/sys/class/net/{dev}/device")).exists()
    }

    /// NetworkManager's ipv4.method, "auto" shown as "dhcp".
    fn method_of(conn: &str) -> Option<String> {
        let m = nmcli(&["-g", "ipv4.method", "connection", "show", conn]).ok()?;
        Some(if m == "auto" { "dhcp".into() } else { m })
    }

    /// Hardware Ethernet and Wi-Fi devices NetworkManager manages, with or without a
    /// cable or an address. Devices managed by something else (docker, libvirt:
    /// "connected (externally)", "unmanaged") are left out. Without nmcli: the
    /// hardware links that have an address, read-only.
    pub fn adapters() -> Vec<Adapter> {
        let mut addrs: Vec<(String, IfAddress, bool)> = Vec::new();
        for i in if_addrs::get_if_addrs().unwrap_or_default() {
            if let if_addrs::IfAddr::V4(a) = &i.addr {
                if !a.ip.is_loopback() {
                    addrs.push((
                        i.name.clone(),
                        IfAddress { ip: a.ip.to_string(), prefix: a.prefixlen },
                        i.is_oper_up(),
                    ));
                }
            }
        }
        let gws = gateways();
        let make = |name: &str, connected: bool, method: String| Adapter {
            id: name.to_string(),
            name: name.to_string(),
            description: String::new(),
            dhcp: match method.as_str() {
                "dhcp" => Some(true),
                "unknown" | "none" => None,
                _ => Some(false),
            },
            method,
            connected,
            addresses: addrs.iter().filter(|(n, _, _)| n == name).map(|(_, a, _)| a.clone()).collect(),
            gateways: gws.iter().filter(|(i, _)| i == name).map(|(_, g)| g.to_string()).collect(),
            wireless: std::path::Path::new(&format!("/sys/class/net/{name}/wireless")).exists(),
        };

        let Ok(devices) = nmcli(&["-t", "-f", "DEVICE,TYPE,STATE", "device"]) else {
            let mut names: Vec<&String> = addrs.iter().filter(|(n, _, _)| is_hardware(n)).map(|(n, _, _)| n).collect();
            names.dedup();
            let up = |n: &str| addrs.iter().any(|(m, _, up)| m == n && *up);
            return names.into_iter().map(|n| make(n, up(n), "unknown".into())).collect();
        };
        devices
            .lines()
            .filter_map(|l| {
                let mut f = l.splitn(3, ':');
                let (dev, kind, state) = (f.next()?, f.next()?, f.next()?);
                let wanted = matches!(kind, "ethernet" | "wifi")
                    && !state.starts_with("unmanaged")
                    && !state.contains("externally")
                    && is_hardware(dev);
                if !wanted {
                    return None;
                }
                let method = match profile_for(dev) {
                    Some(c) => method_of(&c).unwrap_or_else(|| "unknown".into()),
                    None => "none".into(),
                };
                Some(make(dev, state.starts_with("connected"), method))
            })
            .collect()
    }

    /// The active connection, or (no cable) the profile TermWorker made for the device.
    fn profile_for(dev: &str) -> Option<String> {
        connection(dev).or_else(|| {
            let name = format!("TermWorker {dev}");
            nmcli(&["-g", "connection.id", "connection", "show", &name]).ok().map(|_| name)
        })
    }

    pub fn snapshot(id: &str) -> Result<Saved, String> {
        let Some(conn) = profile_for(id) else {
            return Ok(Saved {
                method: "none".into(),
                addresses: Vec::new(),
                gateway: None,
                connection: None,
                never_default: None,
                ignore_auto_dns: None,
                metric: None,
            });
        };
        let get = |field: &str| nmcli(&["-g", field, "connection", "show", &conn]).unwrap_or_default();
        let method = get("ipv4.method");
        let addresses =
            get("ipv4.addresses").split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
        let gateway = Some(get("ipv4.gateway")).filter(|g| !g.is_empty() && g != "--");
        let flag = |field: &str| match get(field).as_str() {
            "yes" => Some(true),
            "no" => Some(false),
            _ => None,
        };
        Ok(Saved {
            method: if method == "auto" { "dhcp".into() } else { method },
            addresses,
            gateway,
            never_default: flag("ipv4.never-default"),
            ignore_auto_dns: flag("ipv4.ignore-auto-dns"),
            connection: Some(conn),
            metric: None,
        })
    }

    fn yes_no(v: bool) -> &'static str {
        if v {
            "yes"
        } else {
            "no"
        }
    }

    /// Puts the connection back as it was; a connection TermWorker made is deleted.
    pub fn restore(id: &str, saved: &Saved) -> Result<String, String> {
        let Some(conn) = &saved.connection else {
            let name = format!("TermWorker {id}");
            return nmcli(&["connection", "delete", "id", &name]);
        };
        let method = if saved.method == "dhcp" { "auto" } else { saved.method.as_str() };
        let addresses = saved.addresses.join(",");
        let gateway = saved.gateway.clone().unwrap_or_default();
        let mut args = vec![
            "connection",
            "modify",
            conn,
            "ipv4.method",
            method,
            "ipv4.addresses",
            &addresses,
            "ipv4.gateway",
            &gateway,
        ];
        if let Some(v) = saved.never_default {
            args.extend(["ipv4.never-default", yes_no(v)]);
        }
        if let Some(v) = saved.ignore_auto_dns {
            args.extend(["ipv4.ignore-auto-dns", yes_no(v)]);
        }
        nmcli(&args)?;
        // Without a cable the profile cannot come up now; it will when the cable is back.
        up(conn)
    }

    /// Activates `conn`; without a cable that waits for the cable, which is not an error.
    fn up(conn: &str) -> Result<String, String> {
        nmcli(&["connection", "up", conn]).or_else(|e| {
            let l = e.to_lowercase();
            if l.contains("no suitable device") || l.contains("no carrier") {
                Ok(String::new())
            } else {
                Err(e)
            }
        })
    }

    /// The connection to change: the active one, else one TermWorker made, else a new one.
    fn connection_for(id: &str) -> Result<String, String> {
        if let Some(c) = connection(id) {
            return Ok(c);
        }
        let name = format!("TermWorker {id}");
        if nmcli(&["-g", "connection.id", "connection", "show", &name]).is_err() {
            let kind = if std::path::Path::new(&format!("/sys/class/net/{id}/wireless")).exists() {
                return Err(format!("{id} is not connected to a Wi-Fi network"));
            } else {
                "ethernet"
            };
            nmcli(&["connection", "add", "type", kind, "ifname", id, "con-name", &name])?;
        }
        Ok(name)
    }

    pub fn set_static(id: &str, ip: Ipv4Addr, prefix: u8, gateway: Option<Ipv4Addr>) -> Result<String, String> {
        let gw = gateway.map(|g| g.to_string()).unwrap_or_default();
        let cidr = format!("{ip}/{prefix}");
        let conn = connection_for(id)?;
        nmcli(&["connection", "modify", &conn, "ipv4.method", "manual", "ipv4.addresses", &cidr, "ipv4.gateway", &gw])?;
        up(&conn)
    }

    /// DHCP; a board link takes no default route and no DNS from the board.
    pub fn set_dhcp(id: &str, board_link: bool) -> Result<String, String> {
        let conn = connection_for(id)?;
        nmcli(&[
            "connection",
            "modify",
            &conn,
            "ipv4.method",
            "auto",
            "ipv4.addresses",
            "",
            "ipv4.gateway",
            "",
            "ipv4.never-default",
            yes_no(board_link),
            "ipv4.ignore-auto-dns",
            yes_no(board_link),
        ])?;
        up(&conn)
    }
}

pub use sys::{adapters, ipv4_addresses};

/// Waits for the change to show, up to `secs`: `done` decides from the adapter list.
fn settle(id: &str, secs: u64, done: impl Fn(&Adapter) -> bool) -> Option<Adapter> {
    let until = std::time::Instant::now() + std::time::Duration::from_secs(secs);
    loop {
        let a = adapters().into_iter().find(|a| a.id == id);
        if a.as_ref().is_some_and(&done) || std::time::Instant::now() >= until {
            return a;
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
}

/// "192.168.1.10/24" → address and prefix.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn parse_cidr(s: &str) -> Option<(Ipv4Addr, u8)> {
    let (ip, prefix) = s.trim().split_once('/')?;
    Some((ip.parse().ok()?, prefix.parse().ok().filter(|p| *p <= 32)?))
}

/// Sets a static address and waits until it is in place (without a cable: until
/// the setting is). Returns how the adapter was before.
pub fn set_static(id: &str, ip: &str, prefix: u8, gateway: Option<&str>) -> Result<Changed, String> {
    let (addr, gw) = validate(ip, prefix, gateway)?;
    let before = sys::snapshot(id)?;
    let output = sys::set_static(id, addr, prefix, gw)?;
    let want = addr.to_string();
    let ok = |a: &Adapter| a.method == "manual" && (!a.connected || a.addresses.iter().any(|x| x.ip == want));
    match settle(id, 8, ok) {
        Some(a) if ok(&a) => Ok(Changed { adapter: a, before }),
        _ => Err(failure("The address was not applied", &output)),
    }
}

/// Back to DHCP. The adapter may take a while to get a lease; that is not an error.
/// A board link (oard_link) takes no default route or DNS from the board.
pub fn set_dhcp(id: &str, board_link: bool) -> Result<Changed, String> {
    let before = sys::snapshot(id)?;
    let output = sys::set_dhcp(id, board_link)?;
    match settle(id, 6, |a| a.method == "dhcp") {
        Some(a) if a.method == "dhcp" => Ok(Changed { adapter: a, before }),
        Some(_) => Err(failure("The adapter is still not on DHCP", &output)),
        None => Err(failure("The adapter is gone", &output)),
    }
}

/// Puts the adapter back as `saved` describes.
pub fn restore(id: &str, saved: &Saved) -> Result<Adapter, String> {
    let output = sys::restore(id, saved)?;
    let ok = |a: &Adapter| a.method == saved.method;
    match settle(id, 8, ok) {
        Some(a) if ok(&a) => Ok(a),
        Some(a) if saved.method == "none" => Ok(a),
        _ => Err(failure("The adapter was not restored", &output)),
    }
}

fn failure(what: &str, output: &str) -> String {
    let detail = output.trim();
    if detail.is_empty() {
        what.to_string()
    } else {
        format!("{what}: {detail}")
    }
}

// ------------------------------------------------------------------ commands

pub mod commands {
    use super::*;

    #[tauri::command]
    pub async fn netcfg_adapters() -> Vec<Adapter> {
        tauri::async_runtime::spawn_blocking(adapters).await.unwrap_or_default()
    }

    /// Blocks on the UAC prompt (Windows) or polkit (Linux): off the main thread.
    #[tauri::command]
    pub async fn netcfg_set_static(
        id: String,
        ip: String,
        prefix: u8,
        gateway: Option<String>,
    ) -> Result<Changed, String> {
        tauri::async_runtime::spawn_blocking(move || set_static(&id, &ip, prefix, gateway.as_deref()))
            .await
            .map_err(|e| e.to_string())?
    }

    #[tauri::command]
    pub async fn netcfg_set_dhcp(id: String, board_link: bool) -> Result<Changed, String> {
        tauri::async_runtime::spawn_blocking(move || set_dhcp(&id, board_link)).await.map_err(|e| e.to_string())?
    }

    #[tauri::command]
    pub async fn netcfg_restore(id: String, saved: Saved) -> Result<Adapter, String> {
        tauri::async_runtime::spawn_blocking(move || restore(&id, &saved)).await.map_err(|e| e.to_string())?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masks() {
        assert_eq!(mask_of(24), Ipv4Addr::new(255, 255, 255, 0));
        assert_eq!(mask_of(16), Ipv4Addr::new(255, 255, 0, 0));
        assert_eq!(mask_of(30), Ipv4Addr::new(255, 255, 255, 252));
        assert_eq!(mask_of(32), Ipv4Addr::new(255, 255, 255, 255));
    }

    #[test]
    fn static_settings_are_checked() {
        assert_eq!(validate("192.168.1.100", 24, None).unwrap(), (Ipv4Addr::new(192, 168, 1, 100), None));
        assert_eq!(
            validate(" 192.168.1.100 ", 24, Some("192.168.1.1")).unwrap().1,
            Some(Ipv4Addr::new(192, 168, 1, 1))
        );
        assert!(validate("192.168.1.100", 24, Some("")).unwrap().1.is_none());
        for (ip, prefix, gw) in [
            ("192.168.1", 24, None),
            ("192.168.1.0", 24, None),
            ("192.168.1.255", 24, None),
            ("127.0.0.1", 8, None),
            ("0.0.0.0", 24, None),
            ("224.0.0.1", 24, None),
            ("192.168.1.100", 0, None),
            ("192.168.1.100", 31, None),
            ("192.168.1.100", 24, Some("192.168.2.1")),
            ("192.168.1.100", 24, Some("x")),
        ] {
            assert!(validate(ip, prefix, gw).is_err(), "{ip}/{prefix} gw {gw:?} should be refused");
        }
    }

    #[test]
    fn cidr_and_virtual_names() {
        assert_eq!(parse_cidr("192.168.1.10/24"), Some((Ipv4Addr::new(192, 168, 1, 10), 24)));
        assert_eq!(parse_cidr(" 10.0.0.1/8 "), Some((Ipv4Addr::new(10, 0, 0, 1), 8)));
        assert_eq!(parse_cidr("10.0.0.1"), None);
        assert_eq!(parse_cidr("10.0.0.1/33"), None);
        assert!(looks_virtual("vEthernet (Default Switch)", "Hyper-V Virtual Ethernet Adapter"));
        assert!(looks_virtual("Ethernet 3", "TAP-Windows Adapter V9"));
        assert!(looks_virtual("Bluetooth Network Connection", "Bluetooth Device (Personal Area Network)"));
        assert!(!looks_virtual("Ethernet", "Realtek PCIe 2.5GbE Family Controller"));
        assert!(!looks_virtual("USB Ethernet", "ASIX AX88179 USB 3.0 to Gigabit Ethernet Adapter"));
    }

    #[test]
    #[ignore = "prints this PC's adapters"]
    fn show_adapters() {
        for a in adapters() {
            println!("ADAPTER {a:?}");
        }
    }
}
