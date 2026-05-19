//! Minimal SSH-agent protocol client (the parts the spec needs: list the
//! agent's identities and ask it to sign a message). The point is to let a
//! node reuse a key that is held by a running agent so the private key never
//! has to be read into this process.
//!
//! This is a deliberately small hand-rolled client over the agent's Unix
//! socket rather than an extra dependency: the protocol surface we use is
//! just two request/response pairs (`REQUEST_IDENTITIES` and
//! `SIGN_REQUEST`), and pulling a full agent-client crate would duplicate the
//! `ssh-key`/tokio stack for no benefit. The framing is the standard
//! SSH-agent protocol (length-prefixed messages, SSH string encoding).
//!
//! The sign path (`SIGN_REQUEST`/`Agent::sign`) is complete and tested but
//! not yet reached from the engine: `Vault` takes an owned in-process
//! `Identity` and signs synchronously, so an agent-held key currently stops
//! at a loud, documented seam (see `idstore::Signer::identity`). The code is
//! kept so wiring the seam later is a local change, not a rewrite.
#![allow(dead_code)]

use anyhow::{anyhow, bail, Context, Result};
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;

const SSH_AGENTC_REQUEST_IDENTITIES: u8 = 11;
const SSH_AGENT_IDENTITIES_ANSWER: u8 = 12;
const SSH_AGENTC_SIGN_REQUEST: u8 = 13;
const SSH_AGENT_SIGN_RESPONSE: u8 = 14;

fn put_u32(v: &mut Vec<u8>, n: u32) {
    v.extend_from_slice(&n.to_be_bytes());
}

fn put_str(v: &mut Vec<u8>, s: &[u8]) {
    put_u32(v, s.len() as u32);
    v.extend_from_slice(s);
}

/// Read a big-endian u32 at `off`, advancing it.
fn take_u32(buf: &[u8], off: &mut usize) -> Result<u32> {
    let end = off
        .checked_add(4)
        .filter(|e| *e <= buf.len())
        .ok_or_else(|| anyhow!("ssh-agent: short reply (u32)"))?;
    let v = u32::from_be_bytes(buf[*off..end].try_into().unwrap());
    *off = end;
    Ok(v)
}

/// Read an SSH `string` (u32 length prefix + bytes) at `off`, advancing it.
fn take_str<'a>(buf: &'a [u8], off: &mut usize) -> Result<&'a [u8]> {
    let n = take_u32(buf, off)? as usize;
    let send = off
        .checked_add(n)
        .filter(|e| *e <= buf.len())
        .ok_or_else(|| anyhow!("ssh-agent: short reply (body)"))?;
    let s = &buf[*off..send];
    *off = send;
    Ok(s)
}

/// One length-framed request → one length-framed reply on the agent socket.
fn round_trip(sock: &mut UnixStream, body: &[u8]) -> Result<Vec<u8>> {
    let mut framed = Vec::with_capacity(body.len() + 4);
    put_u32(&mut framed, body.len() as u32);
    framed.extend_from_slice(body);
    sock.write_all(&framed).context("ssh-agent: write request")?;
    let mut len = [0u8; 4];
    sock.read_exact(&mut len)
        .context("ssh-agent: read reply length")?;
    let n = u32::from_be_bytes(len) as usize;
    let mut reply = vec![0u8; n];
    sock.read_exact(&mut reply)
        .context("ssh-agent: read reply body")?;
    Ok(reply)
}

/// A handle to the running SSH agent named by `SSH_AUTH_SOCK`.
pub struct Agent {
    path: String,
}

impl Agent {
    /// The agent advertised via `SSH_AUTH_SOCK`, if any.
    pub fn from_env() -> Option<Agent> {
        let path = std::env::var("SSH_AUTH_SOCK").ok()?;
        if path.is_empty() {
            return None;
        }
        Some(Agent { path })
    }

    /// Point at a specific agent socket (used by tests that spawn their own
    /// `ssh-agent`; production always goes through `SSH_AUTH_SOCK`).
    #[cfg(test)]
    pub fn for_test(path: &str) -> Agent {
        Agent { path: path.to_string() }
    }

    fn connect(&self) -> Result<UnixStream> {
        UnixStream::connect(&self.path)
            .with_context(|| format!("connect ssh-agent at {}", self.path))
    }

    /// Every `ssh-ed25519` public key the agent currently holds, as its raw
    /// OpenSSH key blob (`string(algo) || string(key)`).
    pub fn ed25519_key_blobs(&self) -> Result<Vec<Vec<u8>>> {
        let mut sock = self.connect()?;
        let reply = round_trip(&mut sock, &[SSH_AGENTC_REQUEST_IDENTITIES])?;
        if reply.first() != Some(&SSH_AGENT_IDENTITIES_ANSWER) {
            bail!("ssh-agent: unexpected reply to identities request");
        }
        let mut off = 1usize;
        let n = take_u32(&reply, &mut off)?;
        let mut out = Vec::new();
        for _ in 0..n {
            let blob = take_str(&reply, &mut off)?.to_vec();
            let _comment = take_str(&reply, &mut off)?;
            let mut bo = 0usize;
            if take_str(&blob, &mut bo)? == b"ssh-ed25519" {
                out.push(blob);
            }
        }
        Ok(out)
    }

    /// Ask the agent to sign `msg` with the key identified by `key_blob`.
    /// Returns the raw 64-byte ed25519 signature (the agent wraps it as
    /// `string(algo) || string(sig)`; we unwrap to the bare signature so it
    /// drops straight into the protocol's detached-signature slot).
    pub fn sign(&self, key_blob: &[u8], msg: &[u8]) -> Result<Vec<u8>> {
        let mut sock = self.connect()?;
        let mut body = vec![SSH_AGENTC_SIGN_REQUEST];
        put_str(&mut body, key_blob);
        put_str(&mut body, msg);
        put_u32(&mut body, 0); // no flags: plain ssh-ed25519
        let reply = round_trip(&mut sock, &body)?;
        match reply.first() {
            Some(&SSH_AGENT_SIGN_RESPONSE) => {}
            _ => bail!(
                "ssh-agent: refused to sign (is the key loaded? `ssh-add -l`)"
            ),
        }
        let mut off = 1usize;
        let wrapped = take_str(&reply, &mut off)?;
        let mut wo = 0usize;
        let algo = take_str(wrapped, &mut wo)?;
        if algo != b"ssh-ed25519" {
            bail!(
                "ssh-agent: signed with {} (expected ssh-ed25519)",
                String::from_utf8_lossy(algo)
            );
        }
        let sig = take_str(wrapped, &mut wo)?;
        if sig.len() != 64 {
            bail!("ssh-agent: ed25519 signature was {} bytes", sig.len());
        }
        Ok(sig.to_vec())
    }
}

/// Extract the 32-byte ed25519 public key from an OpenSSH key blob
/// (`string("ssh-ed25519") || string(pubkey)`).
pub fn ed25519_pubkey_from_blob(blob: &[u8]) -> Result<[u8; 32]> {
    let mut off = 0usize;
    if take_str(blob, &mut off)? != b"ssh-ed25519" {
        bail!("not an ssh-ed25519 key blob");
    }
    let pk = take_str(blob, &mut off)?;
    let arr: [u8; 32] = pk
        .try_into()
        .map_err(|_| anyhow!("ed25519 pubkey was not 32 bytes"))?;
    Ok(arr)
}
