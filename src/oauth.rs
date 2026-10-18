//! "Press ⏎, click Allow, done" sign-in for the apps brief reads (Todoist, Granola).
//!
//! No developer account or pasted token: brief registers itself as an OAuth client on the fly
//! (dynamic client registration), opens the app's consent page, catches the redirect on a
//! one-off local port and trades the code for a token. PKCE, so there is no client secret.
//! Tokens live in one file per app under ~/.brief, readable only by you, and are refreshed
//! when they expire.
//!
//! HTTPS goes through the system `curl`, with tokens passed on stdin rather than the command
//! line.

use crate::store::Store;
use anyhow::{bail, Context, Result};
use chrono::{DateTime, Duration, Local};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Instant;

/// How long to wait for the user to click Allow in the browser.
const CONSENT_WAIT: std::time::Duration = std::time::Duration::from_secs(300);

/// One app brief can sign in to.
pub struct Provider {
    /// "Todoist", in messages.
    pub name: &'static str,
    /// Token file under ~/.brief, e.g. "todoist.json".
    pub file: &'static str,
    pub register: &'static str,
    pub authorize: &'static str,
    pub token: &'static str,
    pub revoke: Option<&'static str>,
    pub scope: &'static str,
    /// The API the token is for (RFC 8707 `resource`), when the app asks for one.
    pub resource: Option<&'static str>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Auth {
    pub client_id: String,
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub expires_at: Option<DateTime<Local>>,
    /// Who connected, e.g. "Manuel (manuel@example.com)", for the connect screen.
    #[serde(default)]
    pub account: String,
    /// The access asked for when connecting ("" for connections older than this field).
    #[serde(default)]
    pub scope: String,
}

impl Provider {
    fn path(&self, store: &Store) -> PathBuf {
        store.root.join(self.file)
    }

    pub fn auth(&self, store: &Store) -> Option<Auth> {
        serde_json::from_str(&std::fs::read_to_string(self.path(store)).ok()?).ok()
    }

    pub fn save(&self, store: &Store, a: &Auth) -> Result<()> {
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new().create(true).write(true).truncate(true).mode(0o600).open(self.path(store))?;
        f.write_all(serde_json::to_string_pretty(a)?.as_bytes())?;
        Ok(())
    }

    /// Opens the consent page and waits until the user allows (or denies) brief. Returns the new
    /// tokens and the raw token response (for an `id_token`); the caller fills in `account` and
    /// saves.
    pub fn connect(&self, store: &Store) -> Result<(Auth, Value)> {
        let name = self.name;
        let listener = TcpListener::bind("127.0.0.1:0").with_context(|| format!("couldn't open a local port for {name} to call back"))?;
        let redirect = format!("http://127.0.0.1:{}/callback", listener.local_addr()?.port());

        let client = json_ok("POST", self.register, None, &[], Some(&json!({
            "client_name": "brief",
            "client_uri": "https://github.com/manoletre/brief",
            "redirect_uris": [redirect],
            "grant_types": ["authorization_code", "refresh_token"],
            "response_types": ["code"],
            "token_endpoint_auth_method": "none",
            "scope": self.scope,
        })))
        .with_context(|| format!("{name} wouldn't register brief"))?;
        let client_id = client["client_id"].as_str().with_context(|| format!("{name} didn't return a client id"))?.to_owned();

        let verifier = random(64);
        let challenge = base64url(&<sha2::Sha256 as sha2::Digest>::digest(verifier.as_bytes()));
        let state = random(24);
        let mut url = format!(
            "{}?response_type=code&client_id={}&scope={}&state={state}&redirect_uri={}&code_challenge={challenge}&code_challenge_method=S256",
            self.authorize,
            enc(&client_id),
            enc(self.scope),
            enc(&redirect),
        );
        if let Some(r) = self.resource {
            let _ = write!(url, "&resource={}", enc(r));
        }
        crate::connect::open(&url);

        let code = wait_for_code(&listener, &state, name)?;
        let mut form = vec![
            ("grant_type", "authorization_code"),
            ("client_id", client_id.as_str()),
            ("code", code.as_str()),
            ("redirect_uri", redirect.as_str()),
            ("code_verifier", verifier.as_str()),
        ];
        if let Some(r) = self.resource {
            form.push(("resource", r));
        }
        let tok = json_ok("POST", self.token, None, &form, None).with_context(|| format!("{name} didn't hand over access"))?;
        let mut a = Auth { client_id, scope: self.scope.to_owned(), ..Default::default() };
        take_tokens(&mut a, &tok, name)?;
        self.save(store, &a)?;
        Ok((a, tok))
    }

    pub fn disconnect(&self, store: &Store) -> Result<String> {
        if let (Some(a), Some(revoke)) = (self.auth(store), self.revoke) {
            // Best effort: forget the token on the app's side too.
            let _ = request("POST", revoke, None, &[], &[("client_id", &a.client_id), ("token", &a.access_token)], None, false);
        }
        let _ = std::fs::remove_file(self.path(store));
        Ok(format!("Disconnected from {}.", self.name))
    }

    /// A token that works right now, refreshed if it has expired (or `force`d, after a 401).
    pub fn access_token(&self, store: &Store, force: bool) -> Result<String> {
        let name = self.name;
        let mut a = self.auth(store).with_context(|| format!("{name} isn't connected"))?;
        let stale = a.expires_at.is_some_and(|t| t - Duration::minutes(2) < Local::now());
        if stale || force {
            let refresh = a.refresh_token.clone().with_context(|| format!("{name} access expired; reconnect {name} in brief"))?;
            let mut form = vec![("grant_type", "refresh_token"), ("client_id", a.client_id.as_str()), ("refresh_token", refresh.as_str())];
            if let Some(r) = self.resource {
                form.push(("resource", r));
            }
            let tok = json_ok("POST", self.token, None, &form, None)
                .with_context(|| format!("{name} access expired and couldn't be renewed; reconnect {name} in brief"))?;
            take_tokens(&mut a, &tok, name)?;
            self.save(store, &a)?;
        }
        Ok(a.access_token)
    }
}

fn take_tokens(a: &mut Auth, tok: &Value, name: &str) -> Result<()> {
    a.access_token = tok["access_token"].as_str().with_context(|| format!("{name} didn't return a token"))?.to_owned();
    if let Some(r) = tok["refresh_token"].as_str() {
        a.refresh_token = Some(r.to_owned());
    }
    a.expires_at = tok["expires_in"].as_i64().map(|s| Local::now() + Duration::seconds(s));
    Ok(())
}

/// "Name (email)", "Name" or "email", whichever parts are there.
pub fn account_name(name: &str, email: &str) -> String {
    match (name.trim(), email.trim()) {
        ("", e) => e.to_owned(),
        (n, "") => n.to_owned(),
        (n, e) => format!("{n} ({e})"),
    }
}

/// The claims inside an OpenID `id_token` (not verified: only used to show who connected).
pub fn id_token_claims(tok: &Value) -> Value {
    let Some(payload) = tok["id_token"].as_str().and_then(|t| t.split('.').nth(1)) else { return Value::Null };
    unbase64url(payload).and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or(Value::Null)
}

// ── The redirect ──

/// Serve the one redirect from the app: `/callback?code=…&state=…` (or `?error=…`).
fn wait_for_code(listener: &TcpListener, state: &str, name: &str) -> Result<String> {
    listener.set_nonblocking(true)?;
    let deadline = Instant::now() + CONSENT_WAIT;
    while Instant::now() < deadline {
        let mut s = match listener.accept() {
            Ok((s, _)) => s,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(std::time::Duration::from_millis(150));
                continue;
            }
            Err(e) => return Err(e.into()),
        };
        s.set_nonblocking(false)?;
        s.set_read_timeout(Some(std::time::Duration::from_secs(5)))?;
        let Some(target) = read_target(&mut s) else { continue };
        let Some(query) = target.strip_prefix("/callback?") else {
            reply(&mut s, "404 Not Found", "");
            continue;
        };
        let params: HashMap<String, String> = query
            .split('&')
            .filter_map(|kv| kv.split_once('='))
            .map(|(k, v)| (dec(k), dec(v)))
            .collect();
        if params.get("state").map(String::as_str) != Some(state) {
            reply(&mut s, "400 Bad Request", &page("That link is out of date", &format!("Go back to brief and press ⏎ on {name} again.")));
            continue;
        }
        if let Some(err) = params.get("error") {
            reply(&mut s, "200 OK", &page("brief wasn't connected", "You can close this tab."));
            bail!("{name} access wasn't given ({err}).");
        }
        let Some(code) = params.get("code") else { continue };
        reply(&mut s, "200 OK", &page(&format!("brief is connected to {name}"), "You can close this tab and go back to brief."));
        return Ok(code.clone());
    }
    bail!("Gave up waiting for {name}. Press ⏎ to try again.")
}

fn read_target(s: &mut TcpStream) -> Option<String> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    while !buf.windows(4).any(|w| w == b"\r\n\r\n") && buf.len() < 64 * 1024 {
        let n = s.read(&mut chunk).ok()?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    let text = String::from_utf8_lossy(&buf);
    let mut parts = text.lines().next()?.split_whitespace();
    (parts.next()? == "GET").then(|| parts.next().map(str::to_owned))?
}

fn reply(s: &mut TcpStream, status: &str, html: &str) {
    let _ = write!(s, "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{html}", html.len());
}

fn page(title: &str, text: &str) -> String {
    format!(
        "<!doctype html><meta charset=utf-8><meta name=viewport content='width=device-width'><title>brief</title>\
<style>body{{font:16px -apple-system,system-ui,sans-serif;background:#141414;color:#eee;display:grid;place-items:center;height:90vh;margin:0}}\
h1{{font-size:22px;color:#ff8700;margin:0 0 8px}}p{{color:#aaa;margin:0}}div{{text-align:center;padding:16px}}</style>\
<div><h1>{title}</h1><p>{text}</p></div>"
    )
}

// ── HTTPS through curl ──

pub struct Response {
    pub code: u16,
    /// Raw header lines, only when asked for.
    pub headers: String,
    pub body: String,
}

impl Response {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.lines().find_map(|l| {
            let (k, v) = l.split_once(':')?;
            k.trim().eq_ignore_ascii_case(name).then(|| v.trim())
        })
    }
}

/// One request. `form` goes in the body for POST, in the query string for GET; `headers` are
/// "Name: value" lines. With `with_headers`, the response headers are kept too.
pub fn request(
    method: &str,
    url: &str,
    token: Option<&str>,
    headers: &[&str],
    form: &[(&str, &str)],
    json_body: Option<&Value>,
    with_headers: bool,
) -> Result<Response> {
    let q = |s: &str| format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""));
    let mut cfg = format!("silent\nshow-error\nmax-time = 60\nwrite-out = \"\\n%{{http_code}}\"\nurl = {}\n", q(url));
    if with_headers {
        cfg.push_str("include\n");
    }
    if method == "GET" {
        cfg.push_str("get\n");
    } else {
        let _ = writeln!(cfg, "request = {}", q(method));
    }
    if let Some(t) = token {
        let _ = writeln!(cfg, "header = {}", q(&format!("Authorization: Bearer {t}")));
    }
    for h in headers {
        let _ = writeln!(cfg, "header = {}", q(h));
    }
    for (k, v) in form {
        let _ = writeln!(cfg, "data-urlencode = {}", q(&format!("{k}={v}")));
    }
    if let Some(body) = json_body {
        let _ = writeln!(cfg, "header = \"Content-Type: application/json\"\ndata = {}", q(&body.to_string()));
    }

    let mut child = Command::new("curl")
        .args(["--config", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("couldn't run curl")?;
    child.stdin.take().unwrap().write_all(cfg.as_bytes())?;
    let out = child.wait_with_output()?;
    let host = url.split('/').nth(2).unwrap_or(url);
    if !out.status.success() {
        bail!("couldn't reach {host}: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let (rest, code) = text.rsplit_once('\n').unwrap_or(("", &text));
    let (headers, body) = if with_headers { split_headers(rest) } else { (String::new(), rest.to_owned()) };
    Ok(Response { code: code.trim().parse().unwrap_or(0), headers, body })
}

/// With `include`, curl prints every header block (e.g. a `100 Continue` first); the last one
/// before the body is the real response.
fn split_headers(raw: &str) -> (String, String) {
    let mut rest = raw;
    let mut headers = String::new();
    while rest.starts_with("HTTP/") {
        let Some(end) = rest.find("\r\n\r\n") else { break };
        headers = rest[..end].to_owned();
        rest = &rest[end + 4..];
    }
    (headers, rest.to_owned())
}

pub fn json_ok(method: &str, url: &str, token: Option<&str>, form: &[(&str, &str)], json_body: Option<&Value>) -> Result<Value> {
    let r = request(method, url, token, &[], form, json_body, false)?;
    let host = url.split('/').nth(2).unwrap_or(url);
    if !(200..300).contains(&r.code) {
        bail!("{host} answered {}: {}", r.code, r.body.chars().take(300).collect::<String>().trim());
    }
    serde_json::from_str(&r.body).with_context(|| format!("unexpected answer from {host}"))
}

// ── Helpers ──

fn random(n: usize) -> String {
    use rand::Rng;
    rand::thread_rng().sample_iter(&rand::distributions::Alphanumeric).take(n).map(char::from).collect()
}

fn base64url(bytes: &[u8]) -> String {
    crate::mcp::base64(bytes).trim_end_matches('=').replace('+', "-").replace('/', "_")
}

fn unbase64url(s: &str) -> Option<Vec<u8>> {
    let mut bits = 0u32;
    let mut n = 0;
    let mut out = Vec::new();
    for c in s.bytes().filter(|c| *c != b'=') {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'-' | b'+' => 62,
            b'_' | b'/' => 63,
            _ => return None,
        } as u32;
        bits = bits << 6 | v;
        n += 6;
        if n >= 8 {
            n -= 8;
            out.push((bits >> n) as u8);
            bits &= (1 << n) - 1;
        }
    }
    Some(out)
}

pub fn enc(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn dec(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < b.len() => match std::str::from_utf8(&b[i + 1..i + 3]).ok().and_then(|h| u8::from_str_radix(h, 16).ok()) {
                Some(v) => {
                    out.push(v);
                    i += 2;
                }
                None => out.push(b'%'),
            },
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_challenge_is_unpadded_base64url_sha256() {
        // SHA-256("abc") = ba7816bf…f20015ad; its base64 has '+' and '/' to swap and '=' to drop.
        assert_eq!(base64url(&<sha2::Sha256 as sha2::Digest>::digest(b"abc")), "ungWv48Bz-pBQUDeXa4iI7ADYaOWF3qctBD_YfIAFa0");
    }

    #[test]
    fn base64url_round_trips() {
        for s in ["", "a", "ab", "abc", "{\"email\":\"m@x.io\"}"] {
            assert_eq!(unbase64url(&base64url(s.as_bytes())).unwrap(), s.as_bytes());
        }
    }

    #[test]
    fn url_coding_round_trips() {
        let s = "http://127.0.0.1:5000/callback?a=b c&é";
        assert_eq!(dec(&enc(s)), s);
        assert_eq!(dec("a+b%2Fc%"), "a b/c%");
    }

    #[test]
    fn last_header_block_wins() {
        let (h, b) = split_headers("HTTP/1.1 100 Continue\r\n\r\nHTTP/2 200\r\nmcp-session-id: abc\r\n\r\n{\"ok\":1}");
        let r = Response { code: 200, headers: h, body: b };
        assert_eq!(r.header("Mcp-Session-Id"), Some("abc"));
        assert_eq!(r.body, "{\"ok\":1}");
    }

    #[test]
    fn callback_ignores_strays_and_returns_the_code() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let browser = std::thread::spawn(move || {
            let hit = |path: &str| {
                let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
                write!(s, "GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n").unwrap();
                let mut out = String::new();
                s.read_to_string(&mut out).unwrap();
                out
            };
            assert!(hit("/favicon.ico").starts_with("HTTP/1.1 404"));
            assert!(hit("/callback?code=evil&state=wrong").starts_with("HTTP/1.1 400"));
            assert!(hit("/callback?code=abc%2F123&state=s1").contains("connected to Todoist"));
        });
        assert_eq!(wait_for_code(&listener, "s1", "Todoist").unwrap(), "abc/123");
        browser.join().unwrap();
    }

    /// Registers a client with each app and loads its consent page. Needs the network:
    /// `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn apps_accept_registration_and_consent_url() {
        for p in [&crate::todoist::TODOIST, &crate::granola::GRANOLA] {
            let redirect = "http://127.0.0.1:59999/callback";
            let client = json_ok("POST", p.register, None, &[], Some(&json!({
                "client_name": "brief", "redirect_uris": [redirect], "grant_types": ["authorization_code", "refresh_token"],
                "response_types": ["code"], "token_endpoint_auth_method": "none", "scope": p.scope,
            })))
            .unwrap();
            let id = client["client_id"].as_str().unwrap();
            let mut url = format!(
                "{}?response_type=code&client_id={}&scope={}&state=x&redirect_uri={}&code_challenge={}&code_challenge_method=S256",
                p.authorize, enc(id), enc(p.scope), enc(redirect), "ungWv48Bz-pBQUDeXa4iI7ADYaOWF3qctBD_YfIAFa0"
            );
            if let Some(r) = p.resource {
                url += &format!("&resource={}", enc(r));
            }
            let r = request("GET", &url, None, &[], &[], None, true).unwrap();
            let location = r.header("location").unwrap_or("");
            assert!(r.code < 400, "{}: consent page answered {}: {}", p.name, r.code, &r.body[..r.body.len().min(300)]);
            // Without a login the app shows (or redirects to) its sign-in page, not back to us with an error.
            assert!(!location.starts_with(redirect) && !r.body.contains("invalid_"), "{}: {location} {}", p.name, &r.body[..r.body.len().min(300)]);
        }
    }
}
