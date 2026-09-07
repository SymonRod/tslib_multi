//! Probe the ICE servers the TeamSpeak desktop client hardcodes.
//!
//! Answers the phase-4 section 9.5 question without a WebRTC stack: do those
//! servers relay (TURN), or only reflect (STUN), and does the local NAT map a
//! socket to one address or to one per destination? Runs on the desktop and,
//! cross-compiled, on the phone over mobile data.
//!
//! No credentials are sent and no media is relayed.

use std::collections::BTreeSet;
use std::net::{IpAddr, SocketAddr, ToSocketAddrs, UdpSocket};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Pin a socket to one Android network, so a probe can leave over mobile data
/// while Wi-Fi stays the default route (and keeps wireless adb alive). The
/// handle comes from `dumpsys connectivity` — the `handle{...}` beside the
/// network id. Without it Android picks the default network, which is exactly
/// what the plain desktop run already measures.
#[cfg(target_os = "android")]
fn bind_to_network(sock: &UdpSocket, handle: u64) -> Result<(), String> {
    use std::ffi::c_void;
    use std::os::fd::AsRawFd;

    extern "C" {
        fn dlopen(filename: *const u8, flag: i32) -> *mut c_void;
        fn dlsym(handle: *mut c_void, symbol: *const u8) -> *mut c_void;
    }

    // android_setsocknetwork lives in libandroid_net.so on device; the NDK
    // stub is libandroid.so. Try both rather than guessing.
    let symbol = unsafe {
        let mut found = std::ptr::null_mut();
        for library in [b"libandroid_net.so\0".as_ptr(), b"libandroid.so\0".as_ptr()] {
            let lib = dlopen(library, 2 /* RTLD_NOW */);
            if !lib.is_null() {
                found = dlsym(lib, b"android_setsocknetwork\0".as_ptr());
                if !found.is_null() {
                    break;
                }
            }
        }
        found
    };
    if symbol.is_null() {
        return Err("android_setsocknetwork not found".into());
    }
    let set: extern "C" fn(u64, i32) -> i32 = unsafe { std::mem::transmute(symbol) };
    if set(handle, sock.as_raw_fd()) == 0 {
        Ok(())
    } else {
        Err(format!("android_setsocknetwork({handle}) failed"))
    }
}

#[cfg(not(target_os = "android"))]
fn bind_to_network(_sock: &UdpSocket, _handle: u64) -> Result<(), String> {
    Err("only available on Android".into())
}

const MAGIC: u32 = 0x2112_A442;
const BINDING_REQUEST: u16 = 0x0001;
const ALLOCATE_REQUEST: u16 = 0x0003;

const ATTR_MAPPED_ADDRESS: u16 = 0x0001;
const ATTR_ERROR_CODE: u16 = 0x0009;
const ATTR_REALM: u16 = 0x0014;
const ATTR_NONCE: u16 = 0x0015;
const ATTR_XOR_MAPPED_ADDRESS: u16 = 0x0020;
const ATTR_SOFTWARE: u16 = 0x8022;
const ATTR_REQUESTED_TRANSPORT: u16 = 0x0019;

/// The two the desktop client hardcodes (phase-4 plan, section 6).
const SERVERS: [(&str, u16); 2] = [("turn.teamspeak.com", 3478), ("turn2.teamspeak.com", 3478)];

/// Unrelated public STUN servers, used only to classify the NAT mapping.
const FALLBACK: [(&str, u16); 2] = [("stun.l.google.com", 19302), ("stun.cloudflare.com", 3478)];

/// A transaction id that needs no dependency: time plus the socket's own port.
fn transaction_id(salt: u16) -> [u8; 12] {
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos() as u64;
    let mut id = [0u8; 12];
    id[..8].copy_from_slice(&nanos.to_be_bytes());
    id[8..10].copy_from_slice(&salt.to_be_bytes());
    id[10..].copy_from_slice(&(nanos as u16).to_be_bytes());
    id
}

fn message(msg_type: u16, attrs: &[u8], salt: u16) -> (Vec<u8>, [u8; 12]) {
    let txid = transaction_id(salt);
    let mut out = Vec::with_capacity(20 + attrs.len());
    out.extend_from_slice(&msg_type.to_be_bytes());
    out.extend_from_slice(&(attrs.len() as u16).to_be_bytes());
    out.extend_from_slice(&MAGIC.to_be_bytes());
    out.extend_from_slice(&txid);
    out.extend_from_slice(attrs);
    (out, txid)
}

fn attribute(attr_type: u16, value: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&attr_type.to_be_bytes());
    out.extend_from_slice(&(value.len() as u16).to_be_bytes());
    out.extend_from_slice(value);
    out.resize(out.len() + (4 - value.len() % 4) % 4, 0);
    out
}

fn parse(data: &[u8], txid: &[u8; 12]) -> Option<(u16, Vec<(u16, Vec<u8>)>)> {
    if data.len() < 20 {
        return None;
    }
    let msg_type = u16::from_be_bytes([data[0], data[1]]);
    let length = u16::from_be_bytes([data[2], data[3]]) as usize;
    let magic = u32::from_be_bytes([data[4], data[5], data[6], data[7]]);
    if magic != MAGIC || &data[8..20] != txid {
        return None;
    }
    let mut attrs = Vec::new();
    let mut body = &data[20..data.len().min(20 + length)];
    while body.len() >= 4 {
        let attr_type = u16::from_be_bytes([body[0], body[1]]);
        let attr_len = u16::from_be_bytes([body[2], body[3]]) as usize;
        if body.len() < 4 + attr_len {
            break;
        }
        attrs.push((attr_type, body[4..4 + attr_len].to_vec()));
        body = &body[(4 + attr_len + (4 - attr_len % 4) % 4).min(body.len())..];
    }
    Some((msg_type, attrs))
}

fn find<'a>(attrs: &'a [(u16, Vec<u8>)], wanted: u16) -> Option<&'a [u8]> {
    attrs.iter().find(|(t, _)| *t == wanted).map(|(_, v)| v.as_slice())
}

/// Decode a MAPPED-ADDRESS or XOR-MAPPED-ADDRESS attribute, IPv4 or IPv6.
fn decode_address(value: &[u8], xor: bool, txid: &[u8; 12]) -> Option<String> {
    if value.len() < 8 {
        return None;
    }
    let family = value[1];
    let mut port = u16::from_be_bytes([value[2], value[3]]);
    if xor {
        port ^= (MAGIC >> 16) as u16;
    }
    // The XOR mask is the magic cookie, extended with the transaction id for v6.
    let mut mask = [0u8; 16];
    mask[..4].copy_from_slice(&MAGIC.to_be_bytes());
    mask[4..].copy_from_slice(txid);

    match family {
        0x01 if value.len() >= 8 => {
            let mut raw = [0u8; 4];
            raw.copy_from_slice(&value[4..8]);
            if xor {
                for (byte, m) in raw.iter_mut().zip(mask.iter()) {
                    *byte ^= m;
                }
            }
            Some(format!("{}:{}", IpAddr::from(raw), port))
        }
        0x02 if value.len() >= 20 => {
            let mut raw = [0u8; 16];
            raw.copy_from_slice(&value[4..20]);
            if xor {
                for (byte, m) in raw.iter_mut().zip(mask.iter()) {
                    *byte ^= m;
                }
            }
            Some(format!("[{}]:{}", IpAddr::from(raw), port))
        }
        _ => None,
    }
}

fn mapped_address(attrs: &[(u16, Vec<u8>)], txid: &[u8; 12]) -> Option<String> {
    if let Some(value) = find(attrs, ATTR_XOR_MAPPED_ADDRESS) {
        return decode_address(value, true, txid);
    }
    find(attrs, ATTR_MAPPED_ADDRESS).and_then(|value| decode_address(value, false, txid))
}

fn error_code(attrs: &[(u16, Vec<u8>)]) -> Option<(u16, String)> {
    let value = find(attrs, ATTR_ERROR_CODE)?;
    if value.len() < 4 {
        return None;
    }
    let code = value[2] as u16 * 100 + value[3] as u16;
    Some((code, String::from_utf8_lossy(&value[4..]).to_string()))
}

fn resolve(host: &str, port: u16, want_v6: bool) -> Option<SocketAddr> {
    (host, port)
        .to_socket_addrs()
        .ok()?
        .find(|addr| addr.is_ipv6() == want_v6)
}

fn exchange(
    sock: &UdpSocket,
    target: SocketAddr,
    payload: &[u8],
    txid: &[u8; 12],
) -> Result<(u16, Vec<(u16, Vec<u8>)>), String> {
    sock.send_to(payload, target).map_err(|e| format!("send failed: {e}"))?;
    let mut buffer = [0u8; 2048];
    for _ in 0..3 {
        match sock.recv_from(&mut buffer) {
            Ok((len, _)) => {
                if let Some(parsed) = parse(&buffer[..len], txid) {
                    return Ok(parsed);
                }
            }
            Err(_) => return Err("no response (timeout)".into()),
        }
    }
    Err("no matching response".into())
}

fn probe_binding(sock: &UdpSocket, target: SocketAddr, salt: u16) -> Result<(String, String), String> {
    let (payload, txid) = message(BINDING_REQUEST, &[], salt);
    let (msg_type, attrs) = exchange(sock, target, &payload, &txid)?;
    if msg_type != 0x0101 {
        return Err(format!("unexpected response type 0x{msg_type:04x}"));
    }
    let address = mapped_address(&attrs, &txid).ok_or("response carried no mapped address")?;
    let software = find(&attrs, ATTR_SOFTWARE)
        .map(|v| String::from_utf8_lossy(v).to_string())
        .unwrap_or_default();
    Ok((address, software))
}

fn probe_allocate(sock: &UdpSocket, target: SocketAddr, salt: u16) -> (bool, String) {
    let attrs = attribute(ATTR_REQUESTED_TRANSPORT, &[17, 0, 0, 0]);
    let (payload, txid) = message(ALLOCATE_REQUEST, &attrs, salt);
    match exchange(sock, target, &payload, &txid) {
        Err(error) => (false, error),
        Ok((0x0113, attrs)) => match error_code(&attrs) {
            // A real TURN server must refuse an unauthenticated Allocate with
            // 401 plus a realm and a nonce.
            Some((401, reason)) if find(&attrs, ATTR_REALM).is_some() && find(&attrs, ATTR_NONCE).is_some() => {
                let realm = String::from_utf8_lossy(find(&attrs, ATTR_REALM).unwrap()).to_string();
                (true, format!("401 {reason}, realm={realm:?}, nonce present"))
            }
            Some((code, reason)) => (false, format!("{code} {reason}")),
            None => (false, "error response without a code".into()),
        },
        Ok((0x0103, _)) => (true, "allocated without credentials (open relay)".into()),
        Ok((msg_type, _)) => (false, format!("unexpected response type 0x{msg_type:04x}")),
    }
}

fn run_family(bind: &str, want_v6: bool, label: &str, network: Option<u64>) -> Vec<String> {
    println!("== {label} ==");
    let sock = match UdpSocket::bind((bind, 0)) {
        Ok(sock) => sock,
        Err(error) => {
            println!("  cannot bind {bind}: {error}\n");
            return Vec::new();
        }
    };
    sock.set_read_timeout(Some(Duration::from_secs(3))).ok();
    if let Some(handle) = network {
        match bind_to_network(&sock, handle) {
            Ok(()) => println!("  pinned to network handle {handle}"),
            Err(error) => {
                println!("  NOT pinned to network {handle}: {error} — this measures the default network");
            }
        }
    }
    let salt = sock.local_addr().map(|a| a.port()).unwrap_or(0);
    println!("  local socket: {}", sock.local_addr().map(|a| a.to_string()).unwrap_or_default());

    let mut reflexive = Vec::new();
    for (host, port) in SERVERS.iter().chain(FALLBACK.iter()) {
        let target = match resolve(host, *port, want_v6) {
            Some(target) => target,
            None => {
                println!("  {host}:{port} — no {label} address in DNS");
                continue;
            }
        };
        match probe_binding(&sock, target, salt) {
            Ok((address, software)) => {
                reflexive.push(address.clone());
                let note = if software.is_empty() { String::new() } else { format!("  [{software}]") };
                println!("  {host}:{port} ({}) — STUN {address}{note}", target.ip());
            }
            Err(error) => println!("  {host}:{port} ({}) — STUN failed: {error}", target.ip()),
        }
        // Only the TeamSpeak pair is asked about relaying; the fallbacks are
        // there to classify the NAT, not to be evaluated as TURN servers.
        if SERVERS.iter().any(|(h, _)| h == host) {
            let (relays, detail) = probe_allocate(&sock, target, salt);
            println!("      TURN relay: {} — {detail}", if relays { "YES" } else { "no" });
        }
    }
    println!();
    reflexive
}

fn classify(reflexive: &[String], label: &str) {
    let unique: BTreeSet<&String> = reflexive.iter().collect();
    if reflexive.len() < 2 {
        println!("{label}: inconclusive, fewer than two reflexive addresses.");
    } else if unique.len() == 1 {
        println!(
            "{label}: one address for every destination ({}) — endpoint-independent mapping. \
             P2P is plausible without a relay.",
            reflexive[0]
        );
    } else {
        let listed: Vec<&str> = unique.iter().map(|s| s.as_str()).collect();
        println!(
            "{label}: the address differs per destination ({}) — SYMMETRIC NAT. \
             P2P needs a TURN relay.",
            listed.join(", ")
        );
    }
}

fn main() {
    println!("ICE server probe — TeamSpeak 6 screen sharing, phase-4 section 9.5\n");
    // An optional Android network handle pins every probe to that network.
    let network = std::env::args().nth(1).and_then(|arg| arg.parse::<u64>().ok());
    if let Some(handle) = network {
        println!("requested network handle: {handle}\n");
    }
    let v4 = run_family("0.0.0.0", false, "IPv4", network);
    let v6 = run_family("::", true, "IPv6", network);
    classify(&v4, "IPv4 NAT mapping");
    classify(&v6, "IPv6 mapping");
}
