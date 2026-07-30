//! Fetches an OIDC attestation token from GCP Confidential Space's local
//! launcher socket. Per `docs/spec-contracts-tee.md` section 3.1, this
//! is the GCP half of `attestation/`; there's no AWS Nitro counterpart
//! in this file, that path uses a COSE_Sign1 document from the Nitro
//! Hypervisor instead, a different enough shape to be its own module
//! when it's built.
//!
//! Verified against a real Intel TDX Confidential Space VM
//! (`docs/gcp-attestation-test-report.md`, 2026-07-30 update): the
//! launcher listens on a Unix socket, not TCP, so this is a hand-rolled
//! minimal HTTP/1.1 client over `UnixStream` rather than a normal HTTP
//! client crate, none of which speak Unix sockets out of the box.
//! `Connection: close` plus reading to EOF sidesteps needing to handle
//! `Content-Length` vs chunked transfer-encoding, `httparse` only does
//! the well-tested header-parsing part.

use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

/// Where the Confidential Space container launcher listens, fixed by
/// GCP's platform, not configurable per
/// <https://cloud.google.com/confidential-computing/confidential-space/docs/create-your-own-workload>.
const SOCKET_PATH: &str = "/run/container_launcher/teeserver.sock";

const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, thiserror::Error)]
pub enum AttestationError {
    #[error("attestation launcher socket not present at {SOCKET_PATH}, not running in Confidential Space")]
    NoLauncherSocket,
    #[error("attestation request timed out after {REQUEST_TIMEOUT:?}")]
    Timeout,
    #[error("i/o error talking to the attestation launcher: {0}")]
    Io(#[from] std::io::Error),
    #[error("attestation launcher returned malformed HTTP: {0}")]
    BadResponse(String),
    #[error("attestation launcher returned HTTP {0}")]
    HttpError(u16),
}

/// Whether the launcher socket exists at all, the cheap presence check
/// `GET /health`'s `attested` field uses. Doesn't verify the socket
/// actually answers, that's what `fetch_oidc_token` does, deliberately
/// kept separate so a liveness probe doesn't pay a full token-fetch
/// round trip on every call.
pub async fn launcher_present() -> bool {
    tokio::fs::try_exists(SOCKET_PATH).await.unwrap_or(false)
}

/// Requests a fresh OIDC attestation token for `audience`. Not cached:
/// tokens are short-lived (~1hr per the observed `iat`/`exp` claims) and
/// `/pubkey` isn't a hot path, so re-fetching per call is simpler than
/// tracking expiry and always returns a currently-valid token.
pub async fn fetch_oidc_token(audience: &str) -> Result<String, AttestationError> {
    if !launcher_present().await {
        return Err(AttestationError::NoLauncherSocket);
    }
    tokio::time::timeout(REQUEST_TIMEOUT, fetch_oidc_token_inner(audience))
        .await
        .map_err(|_| AttestationError::Timeout)?
}

async fn fetch_oidc_token_inner(audience: &str) -> Result<String, AttestationError> {
    let mut stream = UnixStream::connect(SOCKET_PATH).await?;

    let body = serde_json::json!({ "audience": audience, "token_type": "OIDC" }).to_string();
    let request = format!(
        "POST /v1/token HTTP/1.1\r\n\
         Host: localhost\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         \r\n\
         {body}",
        body.len()
    );
    stream.write_all(request.as_bytes()).await?;
    stream.shutdown().await?;

    let mut raw_response = Vec::new();
    stream.read_to_end(&mut raw_response).await?;

    parse_http_response(&raw_response)
}

/// Splits `raw` into status code + body via `httparse`'s header parser,
/// then returns the body trimmed of trailing whitespace (the launcher's
/// response is the bare JWT text, confirmed against the real TDX test
/// run, not JSON-wrapped).
fn parse_http_response(raw: &[u8]) -> Result<String, AttestationError> {
    let mut headers = [httparse::EMPTY_HEADER; 16];
    let mut response = httparse::Response::new(&mut headers);
    let status = response.parse(raw).map_err(|e| AttestationError::BadResponse(e.to_string()))?;
    let header_len = match status {
        httparse::Status::Complete(len) => len,
        httparse::Status::Partial => {
            return Err(AttestationError::BadResponse("incomplete HTTP response".into()))
        }
    };

    let code = response.code.ok_or_else(|| AttestationError::BadResponse("missing status code".into()))?;
    if code != 200 {
        return Err(AttestationError::HttpError(code));
    }

    let body = &raw[header_len..];
    String::from_utf8(body.to_vec())
        .map(|s| s.trim().to_string())
        .map_err(|e| AttestationError::BadResponse(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_real_shaped_response() {
        let raw = b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 3\r\n\r\nabc";
        assert_eq!(parse_http_response(raw).unwrap(), "abc");
    }

    #[test]
    fn trims_trailing_whitespace_from_the_body() {
        let raw = b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nabc\r\n";
        assert_eq!(parse_http_response(raw).unwrap(), "abc");
    }

    #[test]
    fn non_200_status_is_an_error() {
        let raw = b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\n\r\n";
        assert!(matches!(parse_http_response(raw), Err(AttestationError::HttpError(500))));
    }

    #[test]
    fn malformed_response_is_an_error() {
        let raw = b"not an http response at all";
        assert!(matches!(parse_http_response(raw), Err(AttestationError::BadResponse(_))));
    }

    #[tokio::test]
    async fn no_socket_present_returns_the_specific_error_not_a_generic_io_failure() {
        // In any environment this test suite actually runs in (dev
        // machine, CI, GCP-but-not-Confidential-Space), the launcher
        // socket genuinely does not exist, so this exercises the real
        // fallback path, not a mock.
        let result = fetch_oidc_token("cerdic-tee-matcher").await;
        assert!(matches!(result, Err(AttestationError::NoLauncherSocket)));
    }
}
