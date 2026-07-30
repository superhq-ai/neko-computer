use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Result};
use futures_util::{SinkExt, StreamExt};
use http_body_util::{BodyExt, StreamBody};
use hyper::body::{Bytes, Frame};
use hyper::{Method, Request};
use hyper_util::rt::TokioIo;
use serde::{Deserialize, Serialize};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot, Notify};
use tokio_stream::wrappers::UnboundedReceiverStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::header::{HeaderName, HeaderValue};
use tokio_tungstenite::tungstenite::{ClientRequestBuilder, Message};

// Frame types: type(1) + id(4, big-endian) + payload. Must match the edge.
const REQ_HEAD: u8 = 1;
const REQ_BODY: u8 = 2;
const REQ_END: u8 = 3;
const RES_HEAD: u8 = 4;
const RES_BODY: u8 = 5;
const RES_END: u8 = 6;
const ABORT: u8 = 7;
const WS_OPEN: u8 = 8;
// WS_DATA payload is one kind byte (1 text, 0 binary) then the frame data.
const WS_DATA: u8 = 9;
const WS_CLOSE: u8 = 10;

pub type DialRequest = (u16, oneshot::Sender<Result<std::net::TcpStream>>);

/// (rows, cols, reply). Answered by the task that owns the sandbox with a
/// fresh PTY login shell for one web terminal connection.
pub type ShellRequest = (
    u16,
    u16,
    oneshot::Sender<Result<(shuru_sdk::ShellWriter, shuru_sdk::ShellReader)>>,
);

/// The web terminal riding this tunnel. With `exclusive` (no app port is
/// tunneled), every non-terminal path redirects to the terminal.
pub struct TermBridge {
    pub shells: mpsc::UnboundedSender<ShellRequest>,
    pub exclusive: bool,
}

#[derive(Clone)]
pub enum Backend {
    Local(u16),
    Sandbox(mpsc::UnboundedSender<DialRequest>, u16),
}

/// Dev servers disagree about what "localhost" means: Vite and friends often
/// listen on the IPv6 loopback only, so an IPv4-only dial gets connection
/// refused. Try both before giving up.
async fn connect_loopback(port: u16) -> Result<TcpStream> {
    match TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port)).await {
        Ok(s) => Ok(s),
        Err(v4) => match TcpStream::connect((std::net::Ipv6Addr::LOCALHOST, port)).await {
            Ok(s) => Ok(s),
            Err(v6) => Err(anyhow!(
                "nothing is listening on port {port} (127.0.0.1: {v4}; [::1]: {v6})"
            )),
        },
    }
}

impl Backend {
    async fn connect(&self) -> Result<TcpStream> {
        match self {
            Backend::Local(port) => connect_loopback(*port).await,
            Backend::Sandbox(dial, port) => {
                let (reply_tx, reply_rx) = oneshot::channel();
                dial.send((*port, reply_tx))
                    .map_err(|_| anyhow!("sandbox connection closed"))?;
                let std_stream = reply_rx
                    .await
                    .map_err(|_| anyhow!("sandbox dial dropped"))??;
                std_stream.set_nonblocking(true)?;
                Ok(TcpStream::from_std(std_stream)?)
            }
        }
    }

    fn port(&self) -> u16 {
        match self {
            Backend::Local(p) | Backend::Sandbox(_, p) => *p,
        }
    }
}

#[derive(Deserialize)]
struct ReqMeta {
    method: String,
    url: String,
    headers: HashMap<String, String>,
}

#[derive(Serialize)]
struct ResMeta {
    status: u16,
    headers: HashMap<String, String>,
}

#[derive(Deserialize)]
struct WsOpen {
    url: String,
    headers: HashMap<String, String>,
}

enum WsToGuest {
    Text(String),
    Binary(Vec<u8>),
}

#[derive(Deserialize)]
struct TermControl {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    rows: u16,
    #[serde(default)]
    cols: u16,
}

/// `claimed` marks a name the user chose with --subdomain: the edge records a
/// persistent claim for it, while auto-generated names are only held while the
/// connector is live. `ready` runs once the edge has accepted the connector, so
/// callers announce a URL that actually serves rather than one that might be
/// refused a moment later.
pub async fn run(
    subdomain: &str,
    claimed: bool,
    backend: Backend,
    token: &str,
    private_org: Option<&str>,
    term: Option<TermBridge>,
    ready: impl FnOnce(),
) -> Result<()> {
    // The web terminal is a shell into the sandbox; it never rides a tunnel
    // that any visitor can load. The CLI refuses the flag combination too;
    // this is the backstop.
    let term = if private_org.is_some() { term } else { None };
    let domain = crate::dist::domain();
    let mut params: Vec<String> = Vec::new();
    if claimed {
        params.push("claim=1".to_string());
    }
    if let Some(org) = private_org {
        params.push("private=1".to_string());
        params.push(format!("org={org}"));
    }
    let query = if params.is_empty() {
        String::new()
    } else {
        format!("?{}", params.join("&"))
    };
    let url = format!("wss://{subdomain}.{domain}/__neko/connect{query}");
    let request = ClientRequestBuilder::new(url.parse()?)
        .with_header("Authorization", format!("Bearer {token}"));
    let (ws, _) = match tokio_tungstenite::connect_async(request).await {
        Ok(ok) => ok,
        Err(tokio_tungstenite::tungstenite::Error::Http(resp)) => {
            let status = resp.status().as_u16();
            let body = resp
                .into_body()
                .map(|b| String::from_utf8_lossy(&b).trim().to_string())
                .unwrap_or_default();
            if body.is_empty() {
                return Err(anyhow!("edge refused the tunnel (HTTP {status})"));
            }
            return Err(anyhow!("{body}"));
        }
        Err(e) => return Err(e.into()),
    };
    ready();
    let (mut write, mut read) = ws.split();

    let (tx, mut rx) = mpsc::unbounded_channel::<Message>();
    let writer = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if write.send(msg).await.is_err() {
                break;
            }
        }
    });

    let mut bodies: HashMap<u32, mpsc::UnboundedSender<Bytes>> = HashMap::new();
    let aborts: Arc<Mutex<HashMap<u32, Arc<Notify>>>> = Arc::new(Mutex::new(HashMap::new()));
    let sockets: Arc<Mutex<HashMap<u32, mpsc::UnboundedSender<WsToGuest>>>> =
        Arc::new(Mutex::new(HashMap::new()));

    while let Some(msg) = read.next().await {
        let data = match msg? {
            Message::Binary(b) => b,
            Message::Close(_) => break,
            _ => continue,
        };
        if data.len() < 5 {
            continue;
        }
        let t = data[0];
        let id = u32::from_be_bytes([data[1], data[2], data[3], data[4]]);
        let payload = &data[5..];

        match t {
            REQ_HEAD => {
                if let Ok(meta) = serde_json::from_slice::<ReqMeta>(payload) {
                    let path = meta.url.split('?').next().unwrap_or("");
                    if let Some(t) = &term {
                        let is_term_path = (path == crate::term::PREFIX
                            || path.starts_with("/__neko/term/"))
                            && path != crate::term::WS_PATH;
                        if is_term_path {
                            serve_term_asset(id, &meta, path, &tx);
                            continue;
                        }
                        if t.exclusive && path != crate::term::WS_PATH {
                            serve_redirect(id, &tx);
                            continue;
                        }
                    }
                    let (body_tx, body_rx) = mpsc::unbounded_channel::<Bytes>();
                    bodies.insert(id, body_tx);
                    let notify = Arc::new(Notify::new());
                    aborts.lock().unwrap().insert(id, notify.clone());
                    let tx = tx.clone();
                    let backend = backend.clone();
                    let aborts = aborts.clone();
                    tokio::spawn(async move {
                        if let Err(e) = handle(id, meta, body_rx, notify, &tx, &backend).await {
                            let _ = tx.send(Message::Binary(frame(
                                ABORT,
                                id,
                                e.to_string().as_bytes(),
                            )));
                        }
                        aborts.lock().unwrap().remove(&id);
                    });
                }
            }
            REQ_BODY => {
                if let Some(b) = bodies.get(&id) {
                    let _ = b.send(Bytes::copy_from_slice(payload));
                }
            }
            REQ_END => {
                bodies.remove(&id);
            }
            ABORT => {
                bodies.remove(&id);
                if let Some(n) = aborts.lock().unwrap().remove(&id) {
                    n.notify_waiters();
                }
            }
            WS_OPEN => {
                if let Ok(open) = serde_json::from_slice::<WsOpen>(payload) {
                    let (guest_tx, guest_rx) = mpsc::unbounded_channel::<WsToGuest>();
                    sockets.lock().unwrap().insert(id, guest_tx);
                    let tx = tx.clone();
                    let sockets = sockets.clone();
                    let is_term = term.is_some()
                        && open.url.split('?').next().unwrap_or("") == crate::term::WS_PATH;
                    if is_term {
                        let shell_tx = term.as_ref().unwrap().shells.clone();
                        tokio::spawn(async move {
                            if term_ws(id, open, guest_rx, tx.clone(), shell_tx)
                                .await
                                .is_err()
                            {
                                let _ = tx.send(Message::Binary(frame(WS_CLOSE, id, &[])));
                            }
                            sockets.lock().unwrap().remove(&id);
                        });
                    } else {
                        let backend = backend.clone();
                        tokio::spawn(async move {
                            if ws_handle(id, open, guest_rx, tx.clone(), backend)
                                .await
                                .is_err()
                            {
                                let _ = tx.send(Message::Binary(frame(WS_CLOSE, id, &[])));
                            }
                            sockets.lock().unwrap().remove(&id);
                        });
                    }
                }
            }
            WS_DATA => {
                if let Some((&kind, data)) = payload.split_first() {
                    if let Some(guest) = sockets.lock().unwrap().get(&id) {
                        let msg = if kind == 1 {
                            WsToGuest::Text(String::from_utf8_lossy(data).into_owned())
                        } else {
                            WsToGuest::Binary(data.to_vec())
                        };
                        let _ = guest.send(msg);
                    }
                }
            }
            WS_CLOSE => {
                sockets.lock().unwrap().remove(&id);
            }
            _ => {}
        }
    }

    drop(tx);
    let _ = writer.await;
    Ok(())
}

async fn handle(
    id: u32,
    meta: ReqMeta,
    body_rx: mpsc::UnboundedReceiver<Bytes>,
    abort: Arc<Notify>,
    tx: &mpsc::UnboundedSender<Message>,
    backend: &Backend,
) -> Result<()> {
    let io = TokioIo::new(backend.connect().await?);
    let (mut sender, conn) = hyper::client::conn::http1::handshake(io).await?;
    tokio::spawn(async move {
        let _ = conn.await;
    });

    let mut builder = Request::builder()
        .method(Method::from_bytes(meta.method.as_bytes())?)
        .uri(&meta.url);
    for (k, v) in &meta.headers {
        if is_hop_by_hop(k) || k.eq_ignore_ascii_case("host") {
            continue;
        }
        builder = builder.header(k, v);
    }
    builder = builder.header("host", format!("localhost:{}", backend.port()));
    let body = StreamBody::new(
        UnboundedReceiverStream::new(body_rx).map(|chunk| Ok::<_, Infallible>(Frame::data(chunk))),
    );
    let response = sender.send_request(builder.body(body)?).await?;

    let status = response.status().as_u16();
    let mut headers = HashMap::new();
    for (k, v) in response.headers() {
        if is_hop_by_hop(k.as_str()) {
            continue;
        }
        if let Ok(val) = v.to_str() {
            headers.insert(k.as_str().to_string(), val.to_string());
        }
    }
    let head = serde_json::to_vec(&ResMeta { status, headers })?;
    tx.send(Message::Binary(frame(RES_HEAD, id, &head)))
        .map_err(|_| anyhow!("writer gone"))?;

    let mut body = response.into_body();
    let aborted = abort.notified();
    tokio::pin!(aborted);
    loop {
        tokio::select! {
            next = body.frame() => match next {
                Some(Ok(f)) => {
                    if let Ok(chunk) = f.into_data() {
                        if !chunk.is_empty() {
                            tx.send(Message::Binary(frame(RES_BODY, id, &chunk)))
                                .map_err(|_| anyhow!("writer gone"))?;
                        }
                    }
                }
                Some(Err(e)) => return Err(e.into()),
                None => {
                    tx.send(Message::Binary(frame(RES_END, id, &[])))
                        .map_err(|_| anyhow!("writer gone"))?;
                    break;
                }
            },
            _ = &mut aborted => break,
        }
    }
    Ok(())
}

async fn ws_handle(
    id: u32,
    open: WsOpen,
    mut from_edge: mpsc::UnboundedReceiver<WsToGuest>,
    tx: mpsc::UnboundedSender<Message>,
    backend: Backend,
) -> Result<()> {
    let stream = backend.connect().await?;
    let mut request =
        format!("ws://localhost:{}{}", backend.port(), open.url).into_client_request()?;
    let headers = request.headers_mut();
    for (k, v) in &open.headers {
        if skip_ws_header(k) {
            continue;
        }
        if let (Ok(name), Ok(val)) = (
            HeaderName::from_bytes(k.as_bytes()),
            HeaderValue::from_str(v),
        ) {
            headers.insert(name, val);
        }
    }

    let (guest, _) = tokio_tungstenite::client_async(request, stream).await?;
    let (mut guest_write, mut guest_read) = guest.split();

    loop {
        tokio::select! {
            from_e = from_edge.recv() => match from_e {
                Some(WsToGuest::Text(s)) => guest_write.send(Message::Text(s)).await?,
                Some(WsToGuest::Binary(b)) => guest_write.send(Message::Binary(b)).await?,
                None => {
                    let _ = guest_write.send(Message::Close(None)).await;
                    break;
                }
            },
            from_g = guest_read.next() => match from_g {
                Some(Ok(Message::Text(s))) => tx
                    .send(Message::Binary(ws_frame(id, 1, s.as_bytes())))
                    .map_err(|_| anyhow!("writer gone"))?,
                Some(Ok(Message::Binary(b))) => tx
                    .send(Message::Binary(ws_frame(id, 0, &b)))
                    .map_err(|_| anyhow!("writer gone"))?,
                Some(Ok(_)) => {}
                Some(Err(e)) => {
                    let _ = tx.send(Message::Binary(frame(WS_CLOSE, id, &[])));
                    return Err(e.into());
                }
                None => {
                    let _ = tx.send(Message::Binary(frame(WS_CLOSE, id, &[])));
                    break;
                }
            },
        }
    }
    Ok(())
}

/// Terminal-only tunnel: anything that is not the terminal goes to it.
fn serve_redirect(id: u32, tx: &mpsc::UnboundedSender<Message>) {
    let mut headers = HashMap::new();
    headers.insert("location".into(), crate::term::PREFIX.into());
    if let Ok(head) = serde_json::to_vec(&ResMeta {
        status: 302,
        headers,
    }) {
        let _ = tx.send(Message::Binary(frame(RES_HEAD, id, &head)));
        let _ = tx.send(Message::Binary(frame(RES_END, id, &[])));
    }
}

/// Serve one embedded terminal-page asset as a tunnel response.
fn serve_term_asset(id: u32, meta: &ReqMeta, path: &str, tx: &mpsc::UnboundedSender<Message>) {
    let mut headers = HashMap::new();
    let (status, body): (u16, &[u8]) = if meta.method != "GET" && meta.method != "HEAD" {
        headers.insert("allow".into(), "GET, HEAD".into());
        (405, b"method not allowed")
    } else if let Some((bytes, kind, cache)) = crate::term::asset(path) {
        headers.insert("content-type".into(), kind.into());
        headers.insert("cache-control".into(), cache.into());
        if kind.starts_with("text/html") {
            headers.insert(
                "content-security-policy".into(),
                "frame-ancestors 'none'".into(),
            );
        }
        (200, bytes)
    } else {
        (404, b"not found")
    };
    let head = match serde_json::to_vec(&ResMeta { status, headers }) {
        Ok(h) => h,
        Err(_) => return,
    };
    let _ = tx.send(Message::Binary(frame(RES_HEAD, id, &head)));
    if meta.method != "HEAD" && !body.is_empty() {
        let _ = tx.send(Message::Binary(frame(RES_BODY, id, body)));
    }
    let _ = tx.send(Message::Binary(frame(RES_END, id, &[])));
}

/// Bridge one web-terminal WebSocket to a fresh PTY login shell in the
/// sandbox. Binary frames carry PTY bytes both ways; text frames carry JSON
/// control (resize from the page, exit from the shell).
async fn term_ws(
    id: u32,
    open: WsOpen,
    mut guest_rx: mpsc::UnboundedReceiver<WsToGuest>,
    tx: mpsc::UnboundedSender<Message>,
    shell_tx: mpsc::UnboundedSender<ShellRequest>,
) -> Result<()> {
    let query: HashMap<String, u16> = open
        .url
        .split_once('?')
        .map(|(_, q)| {
            q.split('&')
                .filter_map(|kv| {
                    let (k, v) = kv.split_once('=')?;
                    Some((k.to_string(), v.parse().ok()?))
                })
                .collect()
        })
        .unwrap_or_default();
    let rows = *query.get("rows").unwrap_or(&24);
    let cols = *query.get("cols").unwrap_or(&80);

    let (reply_tx, reply_rx) = oneshot::channel();
    shell_tx
        .send((rows, cols, reply_tx))
        .map_err(|_| anyhow!("sandbox gone"))?;
    let (writer, mut reader) = reply_rx.await.map_err(|_| anyhow!("shell dropped"))??;

    loop {
        tokio::select! {
            event = reader.recv() => match event {
                Some(shuru_sdk::ShellEvent::Output(bytes)) => {
                    let mut payload = Vec::with_capacity(bytes.len() + 1);
                    payload.push(0u8);
                    payload.extend_from_slice(&bytes);
                    tx.send(Message::Binary(frame(WS_DATA, id, &payload)))
                        .map_err(|_| anyhow!("writer gone"))?;
                }
                Some(shuru_sdk::ShellEvent::Exit(code)) => {
                    let ctl = format!("{{\"type\":\"exit\",\"code\":{code}}}");
                    let mut payload = vec![1u8];
                    payload.extend_from_slice(ctl.as_bytes());
                    let _ = tx.send(Message::Binary(frame(WS_DATA, id, &payload)));
                    let _ = tx.send(Message::Binary(frame(WS_CLOSE, id, &[])));
                    break;
                }
                Some(shuru_sdk::ShellEvent::Error(_)) | None => {
                    let _ = tx.send(Message::Binary(frame(WS_CLOSE, id, &[])));
                    break;
                }
            },
            msg = guest_rx.recv() => match msg {
                Some(WsToGuest::Binary(bytes)) => { writer.send_input(&bytes)?; }
                Some(WsToGuest::Text(text)) => {
                    if let Ok(ctl) = serde_json::from_str::<TermControl>(&text) {
                        if ctl.kind == "resize" && ctl.rows > 0 && ctl.cols > 0 {
                            let _ = writer.resize(ctl.rows, ctl.cols);
                        }
                    }
                }
                // The visitor socket closed; dropping the writer ends the
                // guest connection and with it the shell session.
                None => break,
            },
        }
    }
    Ok(())
}

fn frame(t: u8, id: u32, payload: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(5 + payload.len());
    v.push(t);
    v.extend_from_slice(&id.to_be_bytes());
    v.extend_from_slice(payload);
    v
}

fn ws_frame(id: u32, kind: u8, data: &[u8]) -> Vec<u8> {
    let mut payload = Vec::with_capacity(1 + data.len());
    payload.push(kind);
    payload.extend_from_slice(data);
    frame(WS_DATA, id, &payload)
}

/// Handshake headers tungstenite sets itself, plus ones that must not cross the
/// relay. Sec-WebSocket-Protocol is kept so the guest can negotiate a subprotocol.
fn skip_ws_header(name: &str) -> bool {
    is_hop_by_hop(name)
        || name.eq_ignore_ascii_case("host")
        || name.eq_ignore_ascii_case("upgrade")
        || name.eq_ignore_ascii_case("sec-websocket-key")
        || name.eq_ignore_ascii_case("sec-websocket-version")
        || name.eq_ignore_ascii_case("sec-websocket-accept")
        || name.eq_ignore_ascii_case("sec-websocket-extensions")
}

/// Headers the connector must not forward on either hop.
fn is_hop_by_hop(name: &str) -> bool {
    name.eq_ignore_ascii_case("connection")
        || name.eq_ignore_ascii_case("keep-alive")
        || name.eq_ignore_ascii_case("proxy-connection")
        || name.eq_ignore_ascii_case("transfer-encoding")
        || name.eq_ignore_ascii_case("content-length")
}
