//! Talking to the service from a client.

use super::{
    PROTOCOL_VERSION, Request, Response,
    wire::{self, RequestEnvelope, ResponseEnvelope},
};
use serde::Deserialize;
use std::{io, os::unix::net::UnixStream, path::Path, time::Duration};

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    /// Nothing listens on the socket (missing, or refusing connections).
    #[error("the PocketSpot service is not running")]
    NotRunning(#[source] io::Error),
    /// Connected, but no complete reply in time: alive but busy or stuck.
    #[error("the PocketSpot service is not answering")]
    NotAnswering,
    #[error("talking to the PocketSpot service failed: {0}")]
    Io(#[source] io::Error),
    /// The reply is not the protocol at all.
    #[error("something other than PocketSpot answers on the control socket")]
    Foreign,
    /// The service speaks another protocol version.
    #[error("the PocketSpot service speaks protocol {service}, this client {PROTOCOL_VERSION}")]
    VersionMismatch { service: u32 },
}

/// Send one request and wait for its reply, for at most
/// [`Request::client_timeout`].
pub fn request(socket: &Path, request: Request) -> Result<Response, ClientError> {
    let timeout = request.client_timeout();
    let envelope = RequestEnvelope {
        protocol: PROTOCOL_VERSION,
        request,
    };
    let line = exchange(socket, &envelope, timeout)?;
    let reply: ResponseEnvelope = wire::decode(&line).ok_or(ClientError::Foreign)?;
    match reply.response {
        Response::VersionMismatch { supported } => {
            Err(ClientError::VersionMismatch { service: supported })
        }
        _ if reply.protocol != PROTOCOL_VERSION => Err(ClientError::VersionMismatch {
            service: reply.protocol,
        }),
        response => Ok(response),
    }
}

/// Who answered [`identify`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Peer {
    pub server: String,
    pub version: String,
    pub pid: u32,
    pub protocol: u32,
}

/// The fields of an identity reply, read leniently so any protocol version
/// decodes.
#[derive(Deserialize)]
struct StableEnvelope {
    response: StableResponse,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum StableResponse {
    Identity {
        server: String,
        version: String,
        pid: u32,
        protocol: u32,
    },
    Accepted,
    #[serde(other)]
    Other,
}

/// Ask whoever listens on `socket` who they are, using only the stable part
/// of the protocol, so any PocketSpot service of any version answers.
pub fn identify(socket: &Path, timeout: Duration) -> Result<Peer, ClientError> {
    let line = exchange(
        socket,
        &stable_request(PROTOCOL_VERSION, "identify"),
        timeout,
    )?;
    match wire::decode::<StableEnvelope>(&line).map(|e| e.response) {
        Some(StableResponse::Identity {
            server,
            version,
            pid,
            protocol,
        }) => Ok(Peer {
            server,
            version,
            pid,
            protocol,
        }),
        _ => Err(ClientError::Foreign),
    }
}

/// Ask a service speaking `protocol`, possibly an older one, to stop.
/// `Ok(true)` if it accepted.
pub fn shutdown(socket: &Path, protocol: u32, timeout: Duration) -> Result<bool, ClientError> {
    let line = exchange(socket, &stable_request(protocol, "shutdown"), timeout)?;
    Ok(matches!(
        wire::decode::<StableEnvelope>(&line).map(|e| e.response),
        Some(StableResponse::Accepted)
    ))
}

fn stable_request(protocol: u32, kind: &str) -> serde_json::Value {
    serde_json::json!({ "protocol": protocol, "request": { "type": kind } })
}

fn exchange(
    socket: &Path,
    message: &impl serde::Serialize,
    timeout: Duration,
) -> Result<String, ClientError> {
    let mut stream = UnixStream::connect(socket).map_err(ClientError::NotRunning)?;
    stream
        .set_read_timeout(Some(timeout))
        .and_then(|()| stream.set_write_timeout(Some(timeout)))
        .map_err(ClientError::Io)?;
    wire::write_message(&mut stream, message).map_err(io_error)?;
    wire::read_message(&mut stream).map_err(io_error)
}

fn io_error(error: io::Error) -> ClientError {
    match error.kind() {
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut => ClientError::NotAnswering,
        io::ErrorKind::InvalidData => ClientError::Foreign,
        _ => ClientError::Io(error),
    }
}
