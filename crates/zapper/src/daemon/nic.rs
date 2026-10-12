//! The daemon end of a session's NIC: connections and DNS queries captured
//! by the namespace shim (`crate::netns`) arrive here over a unix socket.

use std::net::Ipv4Addr;
use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};

use super::session::Session;
use super::{dns, proxy, spec, Daemon, EventBody};

pub async fn serve(d: Arc<Daemon>, s: Arc<Session>, listener: UnixListener) {
    loop {
        let Ok((stream, _)) = listener.accept().await else { break };
        let Ok(permit) = s.conns.clone().try_acquire_owned() else { continue };
        let (d, s) = (d.clone(), s.clone());
        tokio::spawn(async move {
            let _ = handle(d, s, stream).await;
            drop(permit);
        });
    }
}

async fn handle(d: Arc<Daemon>, s: Arc<Session>, mut stream: UnixStream) -> std::io::Result<()> {
    let mut line = Vec::new();
    let mut byte = [0u8; 1];
    while line.len() < 64 {
        stream.read_exact(&mut byte).await?;
        if byte[0] == b'\n' {
            break;
        }
        line.push(byte[0]);
    }
    let line = String::from_utf8_lossy(&line).into_owned();
    let parts: Vec<&str> = line.split(' ').collect();
    match parts.as_slice() {
        ["TCP", ip, port] => {
            let (Ok(ip), Ok(port)) = (ip.parse::<Ipv4Addr>(), port.parse::<u16>()) else { return Ok(()) };
            proxy::transparent(d, s, stream, ip, port).await;
        }
        ["DNS", len] => {
            let len: usize = len.parse().unwrap_or(0);
            if len == 0 || len > 1500 {
                return Ok(());
            }
            let mut query = vec![0u8; len];
            stream.read_exact(&mut query).await?;
            let result = {
                let c = d.config.read().unwrap();
                let layers = s.layers(&c);
                let mut fake = s.dns.lock().unwrap();
                dns::answer(&spec::layers_of(&layers), &mut fake, &query)
            };
            let Some((resp, q, allowed)) = result else { return Ok(()) };
            if q.qtype == dns::TYPE_A || !allowed {
                let answer = (allowed && q.qtype == dns::TYPE_A).then(|| resp[resp.len() - 4..].iter().map(u8::to_string).collect::<Vec<_>>().join("."));
                d.emit(Some(&s), EventBody::Dns { name: q.name, qtype: q.qtype, allowed, answer });
            }
            stream.write_all(&(resp.len() as u16).to_be_bytes()).await?;
            stream.write_all(&resp).await?;
        }
        _ => {}
    }
    Ok(())
}
