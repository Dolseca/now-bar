//! The app notifications API's listener (DOCS.md): loopback HTTP on 20034, or wherever the OS
//! puts it when that is taken; one short thread per connection, each request handed to the UI
//! thread with a channel for its answer. activities/api.rs judges what comes in.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{Ipv4Addr, Ipv6Addr, Shutdown, SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{channel, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use windows::core::PWSTR;
use windows::Win32::Foundation::CloseHandle;
use windows::Win32::NetworkManagement::IpHelper::{GetExtendedTcpTable, MIB_TCP6ROW_OWNER_PID, MIB_TCPROW_OWNER_PID, TCP_TABLE_OWNER_PID_CONNECTIONS};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
use windows::Win32::System::RemoteDesktop::ProcessIdToSessionId;
use windows::Win32::System::Threading::{GetCurrentProcessId, OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION};

use crate::activities::api::{
    discovery_json, events_json, head_end, info_json, ok_json, parse_activity, parse_events, parse_head, parse_notify, pending_json, reject_json, response,
    route, screen, AppRequest, Caller, IconSpec, Kind, RateLimiter, Reject, Route, ASK_WAIT_MS, DEFAULT_PORT, MAX_HEAD,
};
use crate::activities::live::AppIcon;
use crate::platform::log::log;

/// What the UI thread answers a request with: `Shown` first when the request waits for a click
/// (its prompt was answered, its item is up), then `Done`.
#[derive(Debug)]
pub enum Reply {
    Shown,
    Done { status: u16, body: String, retry_after: Option<u32> },
}

impl Reply {
    pub fn reject(r: Reject) -> Reply {
        Reply::Done { status: r.status(), body: reject_json(r, None), retry_after: r.retry_after() }
    }

    pub fn ok(body: String) -> Reply {
        Reply::Done { status: 200, body, retry_after: None }
    }
}

#[derive(Debug)]
pub enum ApiWhat {
    /// A notification or an activity; a notification without an id has been given one.
    Show { req: Box<AppRequest>, icon: Option<AppIcon> },
    Events { wait_s: u32 },
}

/// One request for the UI thread, and where its answer goes.
#[derive(Debug)]
pub struct ApiCall {
    pub caller: Caller,
    pub what: ApiWhat,
    pub reply: Sender<Reply>,
}

pub type ApiMailbox = Arc<Mutex<Vec<ApiCall>>>;

const READ_MS: u64 = 5_000;
const CONNECTIONS: usize = 16;
/// Icons are decoded at this size once; painters scale them to what they draw.
const ICON_PX: u32 = 128;

struct Shared {
    mailbox: ApiMailbox,
    post: fn(),
    port: u16,
    ask_wait: Duration,
    stop: AtomicBool,
    open: AtomicUsize,
    next_id: AtomicU64,
    limiter: Mutex<RateLimiter>,
    /// Exe icons by path (lower case): one shell call per app.
    exe_icons: Mutex<HashMap<String, Option<AppIcon>>>,
}

/// The listener while it runs; dropping it closes the port and takes `api.json` away.
pub struct ApiServer {
    shared: Arc<Shared>,
    discovery: Option<PathBuf>,
}

impl ApiServer {
    /// Listens on 127.0.0.1 and [::1], on 20034 or, when that can't be had, wherever the OS
    /// puts it; `api.json` says where. `post` wakes the UI thread for the mailbox.
    pub fn start(mailbox: ApiMailbox, post: fn()) -> Option<ApiServer> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, DEFAULT_PORT)).or_else(|e| {
            log(&format!("api: port {DEFAULT_PORT} taken ({e}), the OS picks one"));
            TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        });
        let listener = listener.inspect_err(|e| log(&format!("api: can't listen: {e}"))).ok()?;
        let mut server = Self::serve(listener, mailbox, post, Duration::from_millis(ASK_WAIT_MS))?;
        let port = server.port();
        server.discovery = discovery_path().filter(|path| {
            let _ = std::fs::create_dir_all(path.parent().unwrap_or(path));
            std::fs::write(path, discovery_json(port, std::process::id(), crate::platform::updater::VERSION)).is_ok()
        });
        log(&format!("api: listening on 127.0.0.1:{port}"));
        Some(server)
    }

    /// Serves `v4` (and [::1] on the same port, if it can be had).
    fn serve(v4: TcpListener, mailbox: ApiMailbox, post: fn(), ask_wait: Duration) -> Option<ApiServer> {
        let port = v4.local_addr().ok()?.port();
        let shared = Arc::new(Shared {
            mailbox,
            post,
            port,
            ask_wait,
            stop: AtomicBool::new(false),
            open: AtomicUsize::new(0),
            next_id: AtomicU64::new(1),
            limiter: Mutex::default(),
            exe_icons: Mutex::default(),
        });
        let v6 = TcpListener::bind((Ipv6Addr::LOCALHOST, port)).ok();
        for listener in std::iter::once(v4).chain(v6) {
            let shared = shared.clone();
            std::thread::Builder::new().name("api".into()).spawn(move || accept(listener, shared)).ok()?;
        }
        Some(ApiServer { shared, discovery: None })
    }

    pub fn port(&self) -> u16 {
        self.shared.port
    }
}

impl Drop for ApiServer {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        // A blocked accept returns for a connection: each listener sees the flag and closes.
        let port = self.shared.port;
        for addr in [SocketAddr::from((Ipv4Addr::LOCALHOST, port)), SocketAddr::from((Ipv6Addr::LOCALHOST, port))] {
            let _ = TcpStream::connect_timeout(&addr, Duration::from_millis(200));
        }
        if let Some(path) = &self.discovery {
            let _ = std::fs::remove_file(path);
        }
        log("api: stopped");
    }
}

/// `%LOCALAPPDATA%\NowBar\api.json`.
fn discovery_path() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("NowBar").join("api.json"))
}

fn accept(listener: TcpListener, shared: Arc<Shared>) {
    for stream in listener.incoming() {
        if shared.stop.load(Ordering::SeqCst) {
            break;
        }
        let Ok(mut stream) = stream else { continue };
        if shared.open.fetch_add(1, Ordering::SeqCst) >= CONNECTIONS {
            shared.open.fetch_sub(1, Ordering::SeqCst);
            let _ = stream.write_all(&response(503, &reject_json(Reject::Disabled, Some("busy")), Some(1)));
            continue;
        }
        let conn = shared.clone();
        let spawned = std::thread::Builder::new().name("api-conn".into()).spawn(move || {
            // A panic here ends this connection only.
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| handle(stream, &conn)));
            conn.open.fetch_sub(1, Ordering::SeqCst);
        });
        if spawned.is_err() {
            shared.open.fetch_sub(1, Ordering::SeqCst);
        }
    }
}

fn handle(mut stream: TcpStream, shared: &Shared) {
    let answer = serve_one(&mut stream, shared);
    let (status, body, retry) = match answer {
        Reply::Done { status, body, retry_after } => (status, body, retry_after),
        Reply::Shown => (500, String::new(), None),
    };
    let _ = stream.set_write_timeout(Some(Duration::from_millis(READ_MS)));
    let _ = stream.write_all(&response(status, &body, retry));
    let _ = stream.flush();
    let _ = stream.shutdown(Shutdown::Write);
    // Read what the client still sends, so closing doesn't reset the answer away.
    let _ = stream.set_read_timeout(Some(Duration::from_millis(100)));
    let mut sink = [0u8; 4096];
    let mut left = 64 * 1024;
    while let Ok(n @ 1..) = stream.read(&mut sink) {
        left -= n.min(left);
        if left == 0 {
            break;
        }
    }
}

/// One request, read, judged and answered (the UI thread's answer, or the guards').
fn serve_one(stream: &mut TcpStream, shared: &Shared) -> Reply {
    let started = Instant::now();
    let deadline = || Duration::from_millis(READ_MS).checked_sub(started.elapsed()).filter(|d| !d.is_zero());
    let (Ok(peer), Ok(local)) = (stream.peer_addr(), stream.local_addr()) else { return Reply::reject(Reject::Forbidden) };
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 4096];
    let end = loop {
        if let Some(end) = head_end(&buf) {
            break end;
        }
        if buf.len() > MAX_HEAD {
            return Reply::reject(Reject::TooLarge);
        }
        let Some(left) = deadline() else { return Reply::reject(Reject::BadRequest("timeout")) };
        let _ = stream.set_read_timeout(Some(left));
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return Reply::reject(Reject::BadRequest("head")),
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    };
    let head = match parse_head(&buf[..end]) {
        Ok(h) => h,
        Err(r) => return Reply::reject(r),
    };
    if let Err(r) = screen(&head, shared.port) {
        return Reply::reject(r);
    }
    let route = match route(&head.method, &head.path) {
        Ok(r) => r,
        Err(r) => return Reply::reject(r),
    };
    if route == Route::Info {
        return Reply::ok(info_json(crate::platform::updater::VERSION));
    }
    let len = match head.content_length() {
        Ok(Some(n)) => n,
        Ok(None) => return Reply::reject(Reject::LengthRequired),
        Err(r) => return Reply::reject(r),
    };
    let mut body = buf[end..].to_vec();
    while body.len() < len {
        let Some(left) = deadline() else { return Reply::reject(Reject::BadRequest("timeout")) };
        let _ = stream.set_read_timeout(Some(left));
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return Reply::reject(Reject::BadRequest("body")),
            Ok(n) => body.extend_from_slice(&chunk[..n]),
        }
    }
    body.truncate(len);
    let Some((_, exe)) = caller_of(peer, local) else { return Reply::reject(Reject::Forbidden) };
    let (app, what, wait_s) = match parsed(&route, &body) {
        Ok(Parsed::Show(mut req)) => {
            if matches!(req.kind, Kind::Notify { .. }) && req.id.is_none() {
                req.id = Some(format!("n{}", shared.next_id.fetch_add(1, Ordering::SeqCst)));
            }
            let icon = match icon_for(&req.icon, &exe, shared) {
                Ok(icon) => icon,
                Err(r) => return Reply::reject(r),
            };
            let wait_s = req.wait_s;
            (req.app.clone(), ApiWhat::Show { req, icon }, wait_s)
        }
        Ok(Parsed::Events(app, wait_s)) => (app, ApiWhat::Events { wait_s }, wait_s),
        Err(r) => return Reply::reject(r),
    };
    let caller = Caller { app, exe };
    let allowed = shared.limiter.lock().map(|mut l| l.take(&caller.key(), unsafe { windows::Win32::System::SystemInformation::GetTickCount64() })).unwrap_or(false);
    if !allowed {
        return Reply::reject(Reject::RateLimited);
    }
    let client_id = match &what {
        ApiWhat::Show { req, .. } => req.id.clone(),
        ApiWhat::Events { .. } => None,
    };
    let (tx, rx) = channel();
    match shared.mailbox.lock() {
        Ok(mut m) => m.push(ApiCall { caller, what, reply: tx }),
        Err(_) => return Reply::reject(Reject::Disabled),
    }
    (shared.post)();
    let ran_out = || match &client_id {
        Some(id) => Reply::ok(ok_json(id, Some(None))),
        None => Reply::ok(events_json(&[])),
    };
    match rx.recv_timeout(shared.ask_wait) {
        Ok(Reply::Shown) => match rx.recv_timeout(Duration::from_secs(wait_s as u64)) {
            Ok(done @ Reply::Done { .. }) => done,
            _ => ran_out(),
        },
        Ok(done) => done,
        Err(RecvTimeoutError::Timeout) if client_id.is_some() => Reply::Done { status: 202, body: pending_json(), retry_after: None },
        Err(RecvTimeoutError::Timeout) => ran_out(),
        Err(RecvTimeoutError::Disconnected) => Reply::reject(Reject::Disabled),
    }
}

enum Parsed {
    Show(Box<AppRequest>),
    Events(String, u32),
}

fn parsed(route: &Route, body: &[u8]) -> Result<Parsed, Reject> {
    Ok(match route {
        Route::Notify => Parsed::Show(Box::new(parse_notify(body)?)),
        Route::Activity(id) => Parsed::Show(Box::new(parse_activity(id, body)?)),
        Route::Events => {
            let (app, wait) = parse_events(body)?;
            Parsed::Events(app, wait)
        }
        Route::Info => return Err(Reject::NotFound),
    })
}

/// The request's icon, decoded here, off the UI thread.
fn icon_for(spec: &IconSpec, exe: &str, shared: &Shared) -> Result<Option<AppIcon>, Reject> {
    let fnv = |bytes: &[u8]| bytes.iter().fold(0xCBF2_9CE4_8422_2325u64, |h, b| (h ^ *b as u64).wrapping_mul(0x0100_0000_01B3));
    match spec {
        IconSpec::Glyph(c) => Ok(Some(AppIcon::Glyph(*c))),
        IconSpec::Image(bytes) => {
            com();
            let src = crate::render::crest::from_bytes(bytes, ICON_PX).ok_or(Reject::BadRequest("icon"))?;
            Ok(Some(AppIcon::Image { id: fnv(bytes), src }))
        }
        IconSpec::Exe => {
            let key = exe.to_lowercase();
            if let Some(hit) = shared.exe_icons.lock().ok().and_then(|m| m.get(&key).cloned()) {
                return Ok(hit);
            }
            com();
            let icon = crate::render::crest::exe_icon(std::path::Path::new(exe), ICON_PX).map(|src| AppIcon::Image { id: fnv(key.as_bytes()), src });
            // Only what was found is kept: a shell hiccup mustn't leave an app iconless for good.
            if let (Some(found), Ok(mut m)) = (&icon, shared.exe_icons.lock()) {
                if m.len() > 64 {
                    m.clear();
                }
                m.insert(key, Some(found.clone()));
            }
            Ok(icon)
        }
    }
}

/// WIC and the shell's icons want COM on this thread (an STA, as the shelf's thumbnails).
fn com() {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }
}

/// The process behind a loopback connection (`peer` is its end, `local` ours): its id and exe;
/// `None` when it can't be told, or it runs in another session (another Windows user, a service).
pub fn caller_of(peer: SocketAddr, local: SocketAddr) -> Option<(u32, String)> {
    let pid = owner_pid(peer, local).filter(|pid| *pid != 0)?;
    let (mut theirs, mut ours) = (0u32, 0u32);
    unsafe {
        ProcessIdToSessionId(pid, &mut theirs).ok()?;
        ProcessIdToSessionId(GetCurrentProcessId(), &mut ours).ok()?;
    }
    if theirs != ours {
        return None;
    }
    Some((pid, process_exe(pid)?))
}

/// The pid owning the TCP connection whose local end is `peer` and remote end `local`.
fn owner_pid(peer: SocketAddr, local: SocketAddr) -> Option<u32> {
    // The table keeps ports in network order in the low 16 bits.
    let port = |p: u32| u16::from_be(p as u16);
    match peer {
        SocketAddr::V4(_) => {
            let rows = tcp_table::<MIB_TCPROW_OWNER_PID>(2)?;
            rows.iter().find(|r| port(r.dwLocalPort) == peer.port() && port(r.dwRemotePort) == local.port()).map(|r| r.dwOwningPid)
        }
        SocketAddr::V6(_) => {
            let rows = tcp_table::<MIB_TCP6ROW_OWNER_PID>(23)?;
            rows.iter().find(|r| port(r.dwLocalPort) == peer.port() && port(r.dwRemotePort) == local.port()).map(|r| r.dwOwningPid)
        }
    }
}

/// The connections' table with owners for an address family (2 IPv4, 23 IPv6): a count, then
/// the rows.
fn tcp_table<Row: Copy>(family: u32) -> Option<Vec<Row>> {
    for _ in 0..4 {
        let mut size = 0u32;
        unsafe {
            let _ = GetExtendedTcpTable(None, &mut size, false, family, TCP_TABLE_OWNER_PID_CONNECTIONS, 0);
        }
        // Rows are u32s: a u32 buffer keeps them aligned.
        let mut buf = vec![0u32; (size as usize).div_ceil(4) + 16];
        let mut have = (buf.len() * 4) as u32;
        let status = unsafe { GetExtendedTcpTable(Some(buf.as_mut_ptr().cast()), &mut have, false, family, TCP_TABLE_OWNER_PID_CONNECTIONS, 0) };
        // ERROR_INSUFFICIENT_BUFFER: connections came meanwhile; ask again.
        if status == 122 {
            continue;
        }
        if status != 0 {
            return None;
        }
        let count = buf[0] as usize;
        let rows = unsafe { buf.as_ptr().add(1).cast::<Row>() };
        if 4 + count * std::mem::size_of::<Row>() > buf.len() * 4 {
            return None;
        }
        return Some((0..count).map(|i| unsafe { *rows.add(i) }).collect());
    }
    None
}

fn process_exe(pid: u32) -> Option<String> {
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut name = [0u16; 1024];
        let mut len = name.len() as u32;
        let got = QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, PWSTR(name.as_mut_ptr()), &mut len);
        let _ = CloseHandle(process);
        got.ok()?;
        Some(String::from_utf16_lossy(&name[..len as usize]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::{SocketAddr, TcpListener, TcpStream};
    use std::time::Duration;

    fn noop() {}

    fn server(ask_wait: Duration) -> (ApiServer, ApiMailbox) {
        let mailbox: ApiMailbox = Arc::default();
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        (ApiServer::serve(listener, mailbox.clone(), noop, ask_wait).expect("serving"), mailbox)
    }

    fn ask(port: u16, raw: &str) -> String {
        let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        s.write_all(raw.as_bytes()).unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out).unwrap();
        out
    }

    fn post(port: u16, path: &str, body: &str) -> String {
        ask(port, &format!("POST {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", body.len()))
    }

    /// Plays the UI thread: the first call that `wanted` accepts gets `answer` (and is kept
    /// alive `hold` after, so its channel stays open); returns what it saw.
    fn ui(mailbox: ApiMailbox, wanted: fn(&ApiCall) -> bool, answer: Reply, hold: Duration) -> std::thread::JoinHandle<(String, Option<String>, String)> {
        std::thread::spawn(move || loop {
            let call = mailbox.lock().ok().and_then(|mut m| m.iter().position(wanted).map(|i| m.remove(i)));
            if let Some(call) = call {
                let id = match &call.what {
                    ApiWhat::Show { req, .. } => req.id.clone(),
                    ApiWhat::Events { .. } => None,
                };
                let seen = (call.caller.app.clone(), id, call.caller.exe.clone());
                call.reply.send(answer).unwrap();
                std::thread::sleep(hold);
                return seen;
            }
            std::thread::sleep(Duration::from_millis(5));
        })
    }

    #[test]
    fn info_answers() {
        let (s, _) = server(Duration::from_secs(1));
        let r = ask(s.port(), &format!("GET /v1 HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n\r\n", s.port()));
        assert!(r.starts_with("HTTP/1.1 200 OK\r\n"), "{r}");
        assert!(r.contains(r#""name":"Now Bar""#), "{r}");
    }

    #[test]
    fn browser_requests_are_refused_over_the_socket() {
        let (s, mailbox) = server(Duration::from_secs(1));
        let p = s.port();
        let body = r#"{"app":"Ekin","title":"t"}"#;
        let origin = ask(p, &format!("POST /v1/notify HTTP/1.1\r\nHost: 127.0.0.1:{p}\r\nOrigin: https://ornek.test\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", body.len()));
        assert!(origin.starts_with("HTTP/1.1 403"), "{origin}");
        let plain = ask(p, &format!("POST /v1/notify HTTP/1.1\r\nHost: 127.0.0.1:{p}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\n\r\n{body}", body.len()));
        assert!(plain.starts_with("HTTP/1.1 415"), "{plain}");
        let rebound = ask(p, &format!("POST /v1/notify HTTP/1.1\r\nHost: evil.test:{p}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", body.len()));
        assert!(rebound.starts_with("HTTP/1.1 403"), "{rebound}");
        assert!(mailbox.lock().unwrap().is_empty(), "nothing reached the UI");
    }

    #[test]
    fn oversize_body_is_413() {
        let (s, _) = server(Duration::from_secs(1));
        let p = s.port();
        let r = ask(p, &format!("POST /v1/notify HTTP/1.1\r\nHost: 127.0.0.1:{p}\r\nContent-Type: application/json\r\nContent-Length: 200000\r\n\r\n"));
        assert!(r.starts_with("HTTP/1.1 413"), "{r}");
        let r = ask(p, &format!("POST /v1/notify HTTP/1.1\r\nHost: 127.0.0.1:{p}\r\nContent-Type: application/json\r\n\r\n"));
        assert!(r.starts_with("HTTP/1.1 411"), "no length: {r}");
    }

    #[test]
    fn the_caller_is_this_test() {
        let me = std::env::current_exe().unwrap().display().to_string().to_lowercase();
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let _client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (ours, _) = listener.accept().unwrap();
        let (pid, exe) = caller_of(ours.peer_addr().unwrap(), ours.local_addr().unwrap()).expect("found over IPv4");
        assert_eq!((pid, exe.to_lowercase()), (std::process::id(), me.clone()));
        if let Ok(six) = TcpListener::bind(("::1", 0)) {
            let _client = TcpStream::connect(six.local_addr().unwrap()).unwrap();
            let (ours, _) = six.accept().unwrap();
            let (pid, exe) = caller_of(ours.peer_addr().unwrap(), ours.local_addr().unwrap()).expect("found over IPv6");
            assert_eq!((pid, exe.to_lowercase()), (std::process::id(), me));
        }
    }

    #[test]
    fn unknown_caller_is_forbidden() {
        let nobody = |p: u16| SocketAddr::from(([127, 0, 0, 1], p));
        assert_eq!(caller_of(nobody(1), nobody(2)), None);
    }

    #[test]
    fn a_request_reaches_the_mailbox_and_its_reply_goes_back() {
        let (s, mailbox) = server(Duration::from_secs(5));
        let seen = ui(mailbox, |_| true, Reply::Done { status: 200, body: r#"{"ok":true,"id":"x"}"#.into(), retry_after: None }, Duration::ZERO);
        let r = post(s.port(), "/v1/notify", r#"{"app":"Ekin","title":"Merhaba"}"#);
        assert!(r.starts_with("HTTP/1.1 200") && r.ends_with(r#"{"ok":true,"id":"x"}"#), "{r}");
        let (app, id, exe) = seen.join().unwrap();
        assert_eq!(app, "Ekin");
        assert!(id.is_some_and(|id| id.starts_with('n')), "a notification without an id gets one");
        assert_eq!(exe.to_lowercase(), std::env::current_exe().unwrap().display().to_string().to_lowercase());
    }

    #[test]
    fn no_answer_is_pending_and_a_wait_runs_out() {
        let (s, mailbox) = server(Duration::from_millis(200));
        let r = post(s.port(), "/v1/notify", r#"{"app":"Ekin","title":"t"}"#);
        assert!(r.starts_with("HTTP/1.1 202") && r.contains(r#""pending":true"#), "{r}");
        mailbox.lock().unwrap().clear();
        let held = ui(mailbox, |c| matches!(&c.what, ApiWhat::Show { req, .. } if req.id.as_deref() == Some("s1")), Reply::Shown, Duration::from_millis(1500));
        let r = post(s.port(), "/v1/notify", r#"{"app":"Ekin","title":"t","id":"s1","wait":1}"#);
        assert!(r.starts_with("HTTP/1.1 200") && r.contains(r#""event":null"#) && r.contains(r#""id":"s1""#), "{r}");
        held.join().unwrap();
    }

    #[test]
    fn stop_closes_the_port() {
        let (s, _) = server(Duration::from_secs(1));
        let port = s.port();
        drop(s);
        std::thread::sleep(Duration::from_millis(100));
        assert!(TcpStream::connect_timeout(&SocketAddr::from(([127, 0, 0, 1], port)), Duration::from_millis(300)).is_err(), "nothing listens");
    }
}
