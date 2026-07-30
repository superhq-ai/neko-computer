//! Local port forwarding: reach a server inside the sandbox from your own
//! machine. No account, no edge, nothing hosted, so this is the free path for
//! development. `neko run --tunnel` is the same plumbing pointed at the public
//! internet instead.

use anyhow::{anyhow, bail, Result};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot};

use crate::tunnel::DialRequest;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mapping {
    pub host: u16,
    pub guest: u16,
}

/// `8000` maps host 8000 to guest 8000; `3000:8000` maps host 3000 to guest
/// 8000, the same order docker uses.
pub fn parse(spec: &str) -> Result<Mapping> {
    let (host, guest) = match spec.split_once(':') {
        Some((h, g)) => (h.trim(), g.trim()),
        None => (spec.trim(), spec.trim()),
    };
    let host: u16 = host
        .parse()
        .map_err(|_| anyhow!("not a port number: {host}"))?;
    let guest: u16 = guest
        .parse()
        .map_err(|_| anyhow!("not a port number: {guest}"))?;
    if host == 0 || guest == 0 {
        bail!("port must not be zero");
    }
    Ok(Mapping { host, guest })
}

/// Bind the host side and hand each connection to the guest. Returns once the
/// listener is bound, so callers can announce a URL that already accepts.
pub async fn start(map: Mapping, dial: mpsc::UnboundedSender<DialRequest>) -> Result<()> {
    let listener = TcpListener::bind(("127.0.0.1", map.host))
        .await
        .map_err(|e| anyhow!("cannot listen on 127.0.0.1:{}: {e}", map.host))?;

    tokio::spawn(async move {
        loop {
            let Ok((mut local, _)) = listener.accept().await else {
                break;
            };
            let dial = dial.clone();
            let guest_port = map.guest;
            tokio::spawn(async move {
                match connect_guest(&dial, guest_port).await {
                    Ok(mut guest) => {
                        let _ = tokio::io::copy_bidirectional(&mut local, &mut guest).await;
                    }
                    Err(e) => {
                        eprintln!("forward {}: {e}", guest_port);
                    }
                }
            });
        }
    });
    Ok(())
}

async fn connect_guest(dial: &mpsc::UnboundedSender<DialRequest>, port: u16) -> Result<TcpStream> {
    let (reply_tx, reply_rx) = oneshot::channel();
    dial.send((port, reply_tx))
        .map_err(|_| anyhow!("sandbox is gone"))?;
    let std_stream = reply_rx
        .await
        .map_err(|_| anyhow!("sandbox dial dropped"))??;
    std_stream.set_nonblocking(true)?;
    Ok(TcpStream::from_std(std_stream)?)
}
