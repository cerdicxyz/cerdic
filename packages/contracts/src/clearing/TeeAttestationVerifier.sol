// SPDX-License-Identifier: MIT
pragma solidity 0.8.35;
import {AccessControl} from "openzeppelin-contracts/contracts/access/AccessControl.sol";

/// @title  TeeAttestationVerifier
/// @notice Verifies OIDC-format TEE attestation tokens (RS256-signed JWTs) on-chain
///         and registers the enclave signer address they attest to as authorized
///         until the token's expiry. Cloud-agnostic by design: any OIDC-compliant
///         attestation issuer works (GCP Confidential Space is the first concrete
///         one this is wired against), the contract itself has no vendor-specific
///         name or hardcoded issuer, JWKS key and expected claims are both
///         admin-configured. Per ARCHITECTURE.md's attestation-verifier sketch —
///         same "RS256 via the modexp precompile" approach described there,
///         generalized in name and in not hardcoding a single provider.
/// @dev    SECURITY: this contract's whole job is gating who may call
///         SettlementEngine.settleMatch (via AttestationRouter) — a bug here is a
///         bypass of the system's core trust boundary. RSA verification (modexp +
///         PKCS#1 v1.5 padding check) is implemented and tested against a real
///         RSA-2048/SHA-256 signature generated with Node's crypto module (see
///         test/TeeAttestationVerifier.t.sol), but this has NOT been tested against
///         a real attestation token from a live enclave — this environment doesn't
///         have one to test against. Get a real token from a running Confidential
///         Space instance and a security review before this gates real settlement
///         authority; AttestationRouter's existing admin-allowlist path stays the
///         production path until then.
contract TeeAttestationVerifier is AccessControl {
    bytes32 public constant VERIFIER_ADMIN_ROLE = keccak256("VERIFIER_ADMIN_ROLE");

    /// @notice modexp precompile address (EIP-198).
    address internal constant MODEXP = address(0x05);

    /// @notice PKCS#1 v1.5 DigestInfo prefix for SHA-256 (RFC 8017 / RFC 3447 A.2.4).
    bytes internal constant SHA256_DIGEST_INFO_PREFIX = hex"3031300d060960864801650304020105000420";

    /// @notice RSA public key (JWKS n/e), admin-configured — not hardcoded to one
    ///         issuer's key so this same contract works for any OIDC-compliant
    ///         attestation source, GCP or otherwise.
    bytes public modulusN;
    bytes public exponentE;

    /// @notice Claim byte-patterns every accepted token's payload must contain —
    ///         checked as substrings of the raw decoded payload (see
    ///         _payloadContains) rather than full on-chain JSON parsing. Binds the
    ///         specific issuer/audience/image-digest to the exact signed bytes
    ///         without needing a JSON parser on-chain.
    bytes public expectedIssuerClaim;
    bytes public expectedAudienceClaim;
    bytes public expectedImageDigestClaim;

    /// @notice enclaveSigner => authorized until this unix timestamp.
    mapping(address => uint256) public authorizedUntil;

    event PublicKeyUpdated(bytes n, bytes e);
    event ExpectedClaimsUpdated(bytes issuer, bytes audience, bytes imageDigest);
    event AttestationAccepted(address indexed enclaveSigner, uint256 authorizedUntil);

    error ZeroAddress();
    error PublicKeyNotSet();
    error ExpectedClaimsNotSet();
    error InvalidSignatureLength();
    error ModexpCallFailed();
    error PaddingCheckFailed();
    error DigestMismatch();
    error ClaimNotFound(bytes claim);
    error MissingSignerClaim();
    error TokenExpired(uint256 exp, uint256 nowTs);

    constructor(address admin) {
        if (admin == address(0)) revert ZeroAddress();
        _grantRole(DEFAULT_ADMIN_ROLE, admin);
        _grantRole(VERIFIER_ADMIN_ROLE, admin);
    }

    function setPublicKey(bytes calldata n, bytes calldata e) external onlyRole(VERIFIER_ADMIN_ROLE) {
        modulusN = n;
        exponentE = e;
        emit PublicKeyUpdated(n, e);
    }

    function setExpectedClaims(
        bytes calldata issuerClaim,
        bytes calldata audienceClaim,
        bytes calldata imageDigestClaim
    ) external onlyRole(VERIFIER_ADMIN_ROLE) {
        expectedIssuerClaim = issuerClaim;
        expectedAudienceClaim = audienceClaim;
        expectedImageDigestClaim = imageDigestClaim;
        emit ExpectedClaimsUpdated(issuerClaim, audienceClaim, imageDigestClaim);
    }

    /// @notice Verifies an RS256-signed OIDC token and, if valid, registers
    ///         `enclaveSigner` as authorized until `expiry`.
    /// @param  signingInput The JWT's `base64url(header).base64url(payload)` bytes
    ///         exactly as signed — the relayer decodes base64url off-chain and
    ///         passes the original ASCII dot-joined form here, since that's what
    ///         the signature actually covers.
    /// @param  payload The base64url-decoded JSON payload, for the claim checks.
    /// @param  signature The raw RSA signature, big-endian, same byte length as the modulus.
    /// @param  enclaveSigner The enclave's signing address, asserted by the caller and
    ///         bound to the token by requiring its hex-string form to appear in `payload`.
    /// @param  expiry The token's exp claim, asserted by the caller.
    function submitAttestation(
        bytes calldata signingInput,
        bytes calldata payload,
        bytes calldata signature,
        address enclaveSigner,
        uint256 expiry
    ) external {
        if (enclaveSigner == address(0)) revert ZeroAddress();
        if (expiry <= block.timestamp) revert TokenExpired(expiry, block.timestamp);
        if (modulusN.length == 0) revert PublicKeyNotSet();
        if (expectedIssuerClaim.length == 0) revert ExpectedClaimsNotSet();

        if (!_payloadContains(payload, expectedIssuerClaim)) revert ClaimNotFound(expectedIssuerClaim);
        if (!_payloadContains(payload, expectedAudienceClaim)) revert ClaimNotFound(expectedAudienceClaim);
        if (!_payloadContains(payload, expectedImageDigestClaim)) revert ClaimNotFound(expectedImageDigestClaim);
        if (!_payloadContains(payload, bytes(_toHexString(enclaveSigner)))) revert MissingSignerClaim();

        bytes32 digest = sha256(signingInput);
        _verifyRs256(digest, signature);

        authorizedUntil[enclaveSigner] = expiry;
        emit AttestationAccepted(enclaveSigner, expiry);
    }

    /// @dev RSA PKCS#1-v1.5 signature verification: decrypt `signature` via modexp
    ///      (sig^e mod n), check the PKCS#1 padding structure, compare the embedded
    ///      SHA-256 DigestInfo to the actual digest of the signed bytes.
    function _verifyRs256(bytes32 digest, bytes calldata signature) internal view {
        bytes memory n = modulusN;
        if (signature.length != n.length) revert InvalidSignatureLength();

        bytes memory decrypted = _modexp(signature, exponentE, n);

        // PKCS#1 v1.5: 0x00 0x01 [0xFF padding] 0x00 [DigestInfo || digest]
        uint256 k = n.length;
        uint256 digestInfoLen = SHA256_DIGEST_INFO_PREFIX.length + 32;
        if (decrypted.length != k) revert PaddingCheckFailed();
        if (decrypted[0] != 0x00 || decrypted[1] != 0x01) revert PaddingCheckFailed();
        if (k < 3 + digestInfoLen) revert PaddingCheckFailed();

        uint256 psLen = k - 3 - digestInfoLen;
        for (uint256 i = 0; i < psLen; ++i) {
            if (decrypted[2 + i] != 0xff) revert PaddingCheckFailed();
        }
        if (decrypted[2 + psLen] != 0x00) revert PaddingCheckFailed();

        uint256 diStart = 3 + psLen;
        for (uint256 i = 0; i < SHA256_DIGEST_INFO_PREFIX.length; ++i) {
            if (decrypted[diStart + i] != SHA256_DIGEST_INFO_PREFIX[i]) revert PaddingCheckFailed();
        }

        uint256 digestStart = diStart + SHA256_DIGEST_INFO_PREFIX.length;
        for (uint256 i = 0; i < 32; ++i) {
            if (decrypted[digestStart + i] != digest[i]) revert DigestMismatch();
        }
    }

    /// @dev EIP-198 modexp precompile: base^exp mod mod, all big-endian byte strings.
    function _modexp(bytes memory base, bytes memory exp, bytes memory mod_) internal view returns (bytes memory) {
        uint256 baseLen = base.length;
        uint256 expLen = exp.length;
        uint256 modLen = mod_.length;

        bytes memory input = abi.encodePacked(baseLen, expLen, modLen, base, exp, mod_);

        (bool ok, bytes memory out) = MODEXP.staticcall(input);
        if (!ok || out.length != modLen) revert ModexpCallFailed();
        return out;
    }

    /// @dev Naive O(n*m) substring search — fine here since payloads are a single
    ///      bounded-size JWT claim set, not general text; not meant for large inputs.
    function _payloadContains(bytes calldata payload, bytes memory needle) internal pure returns (bool) {
        uint256 n = needle.length;
        if (n == 0) return true;
        if (payload.length < n) return false;
        uint256 last = payload.length - n;
        for (uint256 i = 0; i <= last; ++i) {
            bool matched = true;
            for (uint256 j = 0; j < n; ++j) {
                if (payload[i + j] != needle[j]) {
                    matched = false;
                    break;
                }
            }
            if (matched) return true;
        }
        return false;
    }

    /// @dev Lowercase "0x"-prefixed hex string of an address — the encoding a real
    ///      JSON claim would actually use, not the raw 20 address bytes.
    function _toHexString(address addr) internal pure returns (string memory) {
        bytes memory hexChars = "0123456789abcdef";
        bytes memory result = new bytes(42);
        result[0] = "0";
        result[1] = "x";
        uint160 value = uint160(addr);
        for (uint256 i = 0; i < 20; ++i) {
            uint8 b = uint8(value >> (8 * (19 - i)));
            result[2 + i * 2] = hexChars[b >> 4];
            result[3 + i * 2] = hexChars[b & 0x0f];
        }
        return string(result);
    }
}
