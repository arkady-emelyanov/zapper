//! The guest's DNS server.
//!
//! Names the policy could allow get a synthetic address from 198.18.0.0/15,
//! unique to the name within the session; other names get NXDOMAIN. Nothing
//! is resolved upstream at query time, so queries cannot carry data out. When
//! the guest connects to a synthetic address, the proxy knows the name it
//! stands for and applies name rules to any TCP protocol; it resolves the real
//! address on the host only then, for allowed connections.

use std::collections::HashMap;
use std::net::Ipv4Addr;

use zapper_policy::{resolvable, Layers};

const BASE: u32 = u32::from_be_bytes([198, 18, 0, 0]);
const SIZE: u32 = 1 << 17;

#[derive(Default)]
pub struct FakeDns {
    by_name: HashMap<String, Ipv4Addr>,
    by_ip: HashMap<Ipv4Addr, String>,
}

impl FakeDns {
    pub fn address(&mut self, name: &str) -> Option<Ipv4Addr> {
        if let Some(ip) = self.by_name.get(name) {
            return Some(*ip);
        }
        let n = self.by_name.len() as u32 + 1;
        if n >= SIZE {
            return None;
        }
        let ip = Ipv4Addr::from(BASE + n);
        self.by_name.insert(name.to_string(), ip);
        self.by_ip.insert(ip, name.to_string());
        Some(ip)
    }

    pub fn name(&self, ip: Ipv4Addr) -> Option<&str> {
        self.by_ip.get(&ip).map(String::as_str)
    }
}

pub struct Question {
    pub name: String,
    pub qtype: u16,
}

pub const TYPE_A: u16 = 1;

/// Parses the first question of a query.
pub fn parse(q: &[u8]) -> Option<Question> {
    if q.len() < 12 || q[2] & 0x80 != 0 || u16::from_be_bytes([q[4], q[5]]) == 0 {
        return None;
    }
    let mut i = 12;
    let mut labels = Vec::new();
    loop {
        let len = *q.get(i)? as usize;
        i += 1;
        if len == 0 {
            break;
        }
        if len > 63 {
            return None;
        }
        labels.push(std::str::from_utf8(q.get(i..i + len)?).ok()?.to_ascii_lowercase());
        i += len;
    }
    let qtype = u16::from_be_bytes([*q.get(i)?, *q.get(i + 1)?]);
    q.get(i + 2..i + 4)?;
    let name = labels.join(".");
    if name.len() > 253 {
        return None;
    }
    Some(Question { name, qtype })
}

/// End of the first question, where answers go.
fn question_end(q: &[u8]) -> usize {
    let mut i = 12;
    while i < q.len() && q[i] != 0 {
        i += q[i] as usize + 1;
    }
    i + 5
}

/// A response to `query`: NXDOMAIN, or NOERROR with `answer` (if any).
pub fn respond(query: &[u8], answer: Option<Ipv4Addr>, nxdomain: bool) -> Vec<u8> {
    let end = question_end(query).min(query.len());
    let mut r = query[..end].to_vec();
    let rd = query[2] & 0x01;
    r[2] = 0x80 | (query[2] & 0x78) | 0x04 | rd; // QR, opcode, AA, RD
    r[3] = 0x80 | if nxdomain { 3 } else { 0 }; // RA, rcode
    r[4..6].copy_from_slice(&1u16.to_be_bytes());
    r[6..8].copy_from_slice(&(answer.is_some() as u16).to_be_bytes());
    r[8..12].fill(0);
    if let Some(ip) = answer {
        r.extend_from_slice(&[0xc0, 0x0c]); // name: pointer to the question
        r.extend_from_slice(&TYPE_A.to_be_bytes());
        r.extend_from_slice(&1u16.to_be_bytes()); // IN
        r.extend_from_slice(&60u32.to_be_bytes()); // TTL
        r.extend_from_slice(&4u16.to_be_bytes());
        r.extend_from_slice(&ip.octets());
    }
    r
}

/// Answers a query against the session's policy. Returns the response and
/// whether the name was allowed.
pub fn answer(layers: &Layers<'_>, fake: &mut FakeDns, query: &[u8]) -> Option<(Vec<u8>, Question, bool)> {
    let q = parse(query)?;
    let allowed = resolvable(layers, &q.name);
    let resp = match (allowed, q.qtype) {
        (false, _) => respond(query, None, true),
        (true, TYPE_A) => respond(query, fake.address(&q.name), false),
        // AAAA and the rest: the name exists, without records of that type.
        (true, _) => respond(query, None, false),
    };
    Some((resp, q, allowed))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn query(name: &str, qtype: u16) -> Vec<u8> {
        let mut q = vec![0x12, 0x34, 0x01, 0x00, 0, 1, 0, 0, 0, 0, 0, 0];
        for l in name.split('.') {
            q.push(l.len() as u8);
            q.extend_from_slice(l.as_bytes());
        }
        q.push(0);
        q.extend_from_slice(&qtype.to_be_bytes());
        q.extend_from_slice(&1u16.to_be_bytes());
        q
    }

    #[test]
    fn answers() {
        let config = zapper_policy::Config::from_yaml("policy:\n  net: [{ action: allow, host: '*.example.com' }]\n").unwrap();
        let layers = [(zapper_policy::Scope::Global, &config.policy)];
        let mut fake = FakeDns::default();
        let (r, q, ok) = answer(&layers, &mut fake, &query("API.example.com", TYPE_A)).unwrap();
        assert!(ok && q.name == "api.example.com");
        assert_eq!(&r[..2], &[0x12, 0x34]);
        assert_eq!(r[3] & 0x0f, 0);
        assert_eq!(&r[r.len() - 4..], &[198, 18, 0, 1]);
        assert_eq!(fake.name(Ipv4Addr::new(198, 18, 0, 1)), Some("api.example.com"));
        // Same name, same address.
        let (r2, _, _) = answer(&layers, &mut fake, &query("api.example.com", TYPE_A)).unwrap();
        assert_eq!(&r2[r2.len() - 4..], &[198, 18, 0, 1]);
        let (r, _, ok) = answer(&layers, &mut fake, &query("evil.org", TYPE_A)).unwrap();
        assert!(!ok);
        assert_eq!(r[3] & 0x0f, 3);
        let (r, _, _) = answer(&layers, &mut fake, &query("api.example.com", 28)).unwrap();
        assert_eq!((r[3] & 0x0f, r[7]), (0, 0));
        assert!(parse(&[0; 5]).is_none());
    }
}
