// Forge lifecycle + HTTP server (std only, thread-per-connection).
mod app;
mod git;
mod graphql;
mod store;

use app::{dispatch, parse_query, percent_decode, Request, Response};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::Duration;

fn status_text(code: u16) -> &'static str {
    match code {
        200 => "OK", 201 => "Created", 202 => "Accepted", 204 => "No Content",
        400 => "Bad Request", 401 => "Unauthorized", 404 => "Not Found",
        405 => "Method Not Allowed", 409 => "Conflict", 422 => "Unprocessable Entity",
        500 => "Internal Server Error", _ => "OK",
    }
}

fn read_request(reader: &mut BufReader<TcpStream>) -> Option<Request> {
    let mut line = String::new();
    loop {
        line.clear();
        let n = reader.read_line(&mut line).ok()?;
        if n == 0 { return None; }
        if line == "\r\n" || line == "\n" { continue; }
        break;
    }
    let parts: Vec<&str> = line.trim_end().split_whitespace().collect();
    if parts.len() < 2 { return None; }
    let method = parts[0].to_string();
    let target = parts[1].to_string();
    let mut headers: HashMap<String, String> = HashMap::new();
    loop {
        let mut hline = String::new();
        let n = reader.read_line(&mut hline).ok()?;
        if n == 0 { return None; }
        let t = hline.trim_end();
        if t.is_empty() { break; }
        if let Some(i) = t.find(':') {
            headers.insert(t[..i].trim().to_lowercase(), t[i + 1..].trim().to_string());
        }
    }
    let mut body: Vec<u8> = Vec::new();
    if let Some(te) = headers.get("transfer-encoding") {
        if te.to_lowercase().contains("chunked") {
            loop {
                let mut sz = String::new();
                reader.read_line(&mut sz).ok()?;
                let size = usize::from_str_radix(sz.trim().split(';').next().unwrap_or("0"), 16).unwrap_or(0);
                if size == 0 {
                    loop { let mut l = String::new(); reader.read_line(&mut l).ok()?; if l.trim().is_empty() { break; } }
                    break;
                }
                let start = body.len();
                body.resize(start + size, 0);
                reader.read_exact(&mut body[start..]).ok()?;
                let mut crlf = [0u8; 2]; reader.read_exact(&mut crlf).ok()?;
            }
        }
    } else if let Some(cl) = headers.get("content-length") {
        let len: usize = cl.parse().unwrap_or(0);
        if len > 0 {
            body.resize(len, 0);
            reader.read_exact(&mut body).ok()?;
        }
    }
    let (path, query) = match target.find('?') {
        Some(i) => (target[..i].to_string(), parse_query(&target[i + 1..])),
        None => (target.clone(), HashMap::new()),
    };
    let _ = percent_decode;
    Some(Request { method, path, query, headers, body })
}

fn handle_conn(mut stream: TcpStream, store: Arc<Mutex<store::Store>>) {
    let _ = stream.set_nodelay(true);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(60)));
    let mut reader = BufReader::with_capacity(65536, match stream.try_clone() { Ok(s) => s, Err(_) => return });
    loop {
        let req = match read_request(&mut reader) {
            Some(r) => r,
            None => return,
        };
        let close = req.headers.get("connection").map(|v| v.to_lowercase().contains("close")).unwrap_or(false);
        let resp: Response = {
            let mut s = store.lock().unwrap();
            dispatch(&req, &mut s)
        };
        let mut out = Vec::with_capacity(resp.body.len() + 256);
        out.extend_from_slice(format!("HTTP/1.1 {} {}\r\n", resp.status, status_text(resp.status)).as_bytes());
        for (k, v) in &resp.headers { out.extend_from_slice(format!("{}: {}\r\n", k, v).as_bytes()); }
        out.extend_from_slice(format!("Connection: {}\r\n", if close { "close" } else { "keep-alive" }).as_bytes());
        out.extend_from_slice(b"\r\n");
        out.extend_from_slice(&resp.body);
        if stream.write_all(&out).is_err() { return; }
        if stream.flush().is_err() { return; }
        if close { return; }
    }
}

fn serve(listener: TcpListener, store: Arc<Mutex<store::Store>>) {
    for stream in listener.incoming() {
        match stream {
            Ok(s) => {
                let st = store.clone();
                std::thread::spawn(move || handle_conn(s, st));
            }
            Err(_) => continue,
        }
    }
}

fn env_call(origin: &str, method: &str, path: &str, body: Option<&[u8]>) -> Option<(u16, Vec<u8>)> {
    let stripped = origin.strip_prefix("http://").unwrap_or(origin);
    let (hostport, _) = match stripped.find('/') { Some(i) => (&stripped[..i], &stripped[i..]), None => (stripped, "") };
    let mut stream = TcpStream::connect(hostport).ok()?;
    let _ = stream.set_read_timeout(Some(Duration::from_secs(30)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(30)));
    let mut req = format!("{} {} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n", method, path, hostport);
    if let Some(b) = body {
        req.push_str(&format!("Content-Type: application/json\r\nContent-Length: {}\r\n", b.len()));
    } else {
        req.push_str("Content-Length: 0\r\n");
    }
    req.push_str("\r\n");
    stream.write_all(req.as_bytes()).ok()?;
    if let Some(b) = body { stream.write_all(b).ok()?; }
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).ok()?;
    let text = String::from_utf8_lossy(&raw);
    let status: u16 = text.split_whitespace().nth(1).and_then(|s| s.parse().ok()).unwrap_or(0);
    let body_start = text.find("\r\n\r\n").map(|i| i + 4).unwrap_or(text.len());
    Some((status, text[body_start..].as_bytes().to_vec()))
}

fn main() {
    let store = Arc::new(Mutex::new(store::Store::new()));
    let env_url = std::env::var("MAKEFUN_ENV_URL").unwrap_or_default().trim_end_matches('/').to_string();
    if env_url.is_empty() {
        let port: u16 = std::env::args().nth(1).and_then(|a| a.parse().ok())
            .or_else(|| std::env::var("PORT").ok().and_then(|p| p.parse().ok()))
            .unwrap_or(8787);
        let listener = TcpListener::bind(("127.0.0.1", port)).expect("bind dev port");
        eprintln!("[forge] dev listening on 127.0.0.1:{}", port);
        serve(listener, store);
        return;
    }
    let listener = TcpListener::bind(("0.0.0.0", 0)).expect("bind ephemeral port");
    let port = listener.local_addr().unwrap().port();
    eprintln!("[forge] listening on port {}", port);
    let store2 = store.clone();
    std::thread::spawn(move || serve(listener, store2));
    let mut failures = 0u32;
    loop {
        let body = format!("{{\"port\":{}}}", port);
        match env_call(&env_url, "POST", "/register", Some(body.as_bytes())) {
            Some((200, _)) | Some((409, _)) => break,
            Some((400, _)) | Some((405, _)) => { eprintln!("[forge] register refused"); std::process::exit(1); }
            _ => {}
        }
        let pause = std::cmp::min(2000, 250 * (1 << std::cmp::min(failures, 3)));
        std::thread::sleep(Duration::from_millis(pause));
        failures += 1;
    }
    eprintln!("[forge] announced; waiting for the trial to finish");
    let mut failures = 0u32;
    loop {
        match env_call(&env_url, "GET", "/done", None) {
            Some((200, b)) => {
                let s = String::from_utf8_lossy(&b);
                if s.contains("\"done\":true") || s.contains("\"done\": true") {
                    eprintln!("[forge] trial finished");
                    std::process::exit(0);
                }
            }
            _ => {}
        }
        let pause = std::cmp::min(2000, 250 * (1 << std::cmp::min(failures, 3)));
        std::thread::sleep(Duration::from_millis(pause));
        failures += 1;
    }
}
