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

/// Requests a fresh OIDC attestation token for `audience`, optionally
/// binding `nonce` into the token's `eat_nonce` claim (GCP Confidential
/// Space's `nonces` request field, 10-74 bytes per entry). This is how
/// this enclave's settlement-signing address gets cryptographically tied
/// to a specific attested boot: the real token has no claim carrying an
/// arbitrary address on its own (`sub` is the instance URL, not a key),
/// so `TeeAttestationVerifier.sol`'s enclave-signer check only means
/// anything if the address is bound in via a nonce at request time. Not
/// cached: tokens are short-lived (~1hr per the observed `iat`/`exp`
/// claims) and `/pubkey` isn't a hot path, so re-fetching per call is
/// simpler than tracking expiry and always returns a currently-valid
/// token.
pub async fn fetch_oidc_token(audience: &str, nonce: Option<&str>) -> Result<String, AttestationError> {
    if !launcher_present().await {
        return Err(AttestationError::NoLauncherSocket);
    }
    tokio::time::timeout(REQUEST_TIMEOUT, fetch_oidc_token_inner(audience, nonce))
        .await
        .map_err(|_| AttestationError::Timeout)?
}

async fn fetch_oidc_token_inner(audience: &str, nonce: Option<&str>) -> Result<String, AttestationError> {
    let mut stream = UnixStream::connect(SOCKET_PATH).await?;

    let mut req = serde_json::json!({ "audience": audience, "token_type": "OIDC" });
    if let Some(n) = nonce {
        req["nonces"] = serde_json::json!([n]);
    }
    let body = req.to_string();
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
/// response is the bare JWT text, not JSON-wrapped).
///
/// The launcher sends `Transfer-Encoding: chunked` (confirmed against a
/// real TDX test run: an earlier version of this function assumed the
/// body could just be taken as-is after the headers, and the "token" it
/// produced turned out to have chunk-framing bytes glued onto it, e.g. a
/// leading `939\r\n` chunk-size line and a trailing `\r\n0\r\n\r\n`
/// terminator, both very much not part of the JWT). Declaring
/// `Connection: close` and reading to EOF sidesteps needing
/// `Content-Length`, but does not make chunked framing go away if the
/// server sends it anyway, still has to be decoded either way.
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

    let is_chunked = response
        .headers
        .iter()
        .any(|h| h.name.eq_ignore_ascii_case("transfer-encoding") && contains_ci(h.value, b"chunked"));

    let body = &raw[header_len..];
    let decoded = if is_chunked { decode_chunked(body)? } else { body.to_vec() };

    String::from_utf8(decoded)
        .map(|s| s.trim().to_string())
        .map_err(|e| AttestationError::BadResponse(e.to_string()))
}

fn contains_ci(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.to_ascii_lowercase().windows(needle.len()).any(|w| w.eq_ignore_ascii_case(needle))
}

/// Decodes an HTTP/1.1 chunked-transfer body: repeating `<hex
/// size>\r\n<size bytes>\r\n`, terminated by a `0\r\n` chunk followed by
/// an (ignored, no trailers expected here) trailer section.
fn decode_chunked(mut body: &[u8]) -> Result<Vec<u8>, AttestationError> {
    let mut out = Vec::new();
    loop {
        let line_end = find_crlf(body)
            .ok_or_else(|| AttestationError::BadResponse("chunked body: missing size line".into()))?;
        let size_line = std::str::from_utf8(&body[..line_end])
            .map_err(|e| AttestationError::BadResponse(format!("chunked body: {e}")))?;
        // Ignore chunk extensions (";...") if present, only the size matters.
        let size_hex = size_line.split(';').next().unwrap_or(size_line).trim();
        let size = usize::from_str_radix(size_hex, 16).map_err(|e| {
            AttestationError::BadResponse(format!("chunked body: bad size {size_hex:?}: {e}"))
        })?;

        body = &body[line_end + 2..];
        if size == 0 {
            break;
        }
        if body.len() < size + 2 {
            return Err(AttestationError::BadResponse("chunked body: truncated chunk".into()));
        }
        out.extend_from_slice(&body[..size]);
        body = &body[size + 2..]; // skip chunk data + its trailing CRLF
    }
    Ok(out)
}

fn find_crlf(buf: &[u8]) -> Option<usize> {
    buf.windows(2).position(|w| w == b"\r\n")
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
        let result = fetch_oidc_token("cerdic-tee-matcher", None).await;
        assert!(matches!(result, Err(AttestationError::NoLauncherSocket)));
    }

    /// Reproduces the exact shape observed from a real Confidential
    /// Space launcher response: `Transfer-Encoding: chunked`, one chunk
    /// holding the JWT, terminated by a zero-size chunk. Before the
    /// chunked-decode fix, this token came back with `939\r\n` glued to
    /// the front and `\r\n0` to the back, real bytes captured from
    /// `docs/gcp-attestation-test-report.md`'s 2026-08-04 test run.
    #[test]
    fn decodes_a_real_shaped_chunked_response() {
        let jwt = "eyJhbGciOiJSUzI1NiJ9.eyJhdWQiOiJjZXJkaWMtdGVlLW1hdGNoZXIifQ.sig";
        let raw = format!(
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n{jwt}\r\n0\r\n\r\n",
            jwt.len()
        );
        assert_eq!(parse_http_response(raw.as_bytes()).unwrap(), jwt);
    }

    #[test]
    fn decodes_a_chunked_response_split_across_multiple_chunks() {
        let raw =
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n6\r\n world\r\n0\r\n\r\n";
        assert_eq!(parse_http_response(raw).unwrap(), "hello world");
    }
}
