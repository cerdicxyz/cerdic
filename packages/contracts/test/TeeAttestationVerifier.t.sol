// SPDX-License-Identifier: MIT
pragma solidity 0.8.35;
import {Test} from "forge-std/Test.sol";
import {TeeAttestationVerifier} from "../src/clearing/TeeAttestationVerifier.sol";

/// @title  TeeAttestationVerifierTest
/// @notice Tests the on-chain RS256/modexp verification against a REAL RSA-2048
///         keypair and a genuine RS256 signature, both generated with Node's
///         crypto module (not fabricated/mocked) — see the generation script in
///         this session's history for exact reproduction steps. This is a
///         synthetic JWT-shaped payload, not a real GCP Confidential Space token
///         (this environment has no live enclave to source one from) — see
///         TeeAttestationVerifier.sol's own module doc for what that gap means
///         before this gates real settlement authority.
contract TeeAttestationVerifierTest is Test {
    TeeAttestationVerifier internal verifier;
    address internal admin = makeAddr("admin");

    // Real RSA-2048 public key (n, e) generated via Node crypto.generateKeyPairSync.
    bytes internal constant N =
        hex"a6791650fc844cc0572491273e7287f9541ee8d927d295b7578bb7f908d61c790037767e154d2a6549b5dc1e09f3d62d1ed77ded54d904b3493602afb5fa79799ecd70741a24608f544716cbf399a8eab0f6d5076b2c6bcf78047f429b50cf5755acc262815a0a49ace3997f1fd314585bea8cf5d0b31562ef178cfe0e92ab246f0a945b4183419fd93013909d489854b204bebeb3e299f0542bd487c0b9cecece930c4cfa9509f8e981ea4b5750a036a8e69f9358e587282db913f3ac9482b55f6ffe6cf7f01f875e5df81e488a9b70dcd0db276c1cd6cbaeed061c2c9b9bf843bb1393f9bc9fecde1e9255de5920b45c960b4f5fb42087521f6e13e40ab3ab";
    bytes internal constant E = hex"010001";

    // Real RS256 signature (RSASSA-PKCS1-v1_5, SHA-256) over SIGNING_INPUT below.
    bytes internal constant SIG =
        hex"94c283434d8965632c20ac17a09f4e5eb280fc2254aad6ec16068d9cdc22818832a2fb72c6e12fee89f9b2565b14bb87cfe5f683bb95aa0737580dcac37c9910cb7ae32f33e94a2e18459f5f791359ae08c8144ada9cb99028787239ca950e12775e58bb827a24e53a0c10c64c87098b670909851b6ba73a5ab6ee5d1a474e4946d2b35157189d4f3aaa9481bff73ca2a81e5cbff9507675b008b7169f18e73ac69173f7307ab0448241919b359732ed72daa03a98a5a3189a2700a07970b1d09d5da15b3aa515f2c83b20e8c34fd4efaf0c778bdd73b77622f03895ddf39ea3200409bc594cdafc679ee6c818c47149afa48ef22beeb64cb0d2bdc461071e2b";

    // base64url(header) + "." + base64url(payload), the exact bytes RS256 signs.
    bytes internal constant SIGNING_INPUT =
        "eyJhbGciOiJSUzI1NiIsInR5cCI6IkpXVCJ9.eyJpc3MiOiJodHRwczovL2FjY291bnRzLmdvb2dsZS5jb20iLCJhdWQiOiJjZXJkaWMtdGVlLW1hdGNoZXIiLCJpbWFnZV9kaWdlc3QiOiJzaGEyNTY6ZGVhZGJlZWYwMDExMjIzMzQ0NTU2Njc3ODg5OWFhYmJjY2RkZWVmZjAwMTEyMjMzNDQ1NTY2Nzc4ODk5MDAiLCJzdWIiOiIweDEyMzQ1Njc4OTBhYmNkZWYxMjM0NTY3ODkwYWJjZGVmMTIzNDU2NzgiLCJleHAiOjk5OTk5OTk5OTl9";

    // The decoded JSON payload (base64url-decoded), for the claim substring checks.
    bytes internal constant PAYLOAD =
        '{"iss":"https://accounts.google.com","aud":"cerdic-tee-matcher","image_digest":"sha256:deadbeef00112233445566778899aabbccddeeff0011223344556677889900","sub":"0x1234567890abcdef1234567890abcdef12345678","exp":9999999999}';

    bytes internal constant ISSUER = "https://accounts.google.com";
    bytes internal constant AUDIENCE = "cerdic-tee-matcher";
    bytes internal constant IMAGE_DIGEST = "sha256:deadbeef00112233445566778899aabbccddeeff0011223344556677889900";
    address internal constant ENCLAVE_SIGNER = 0x1234567890AbcdEF1234567890aBcdef12345678;
    uint256 internal constant EXP = 9999999999;

    function setUp() public {
        verifier = new TeeAttestationVerifier(admin);
        vm.startPrank(admin);
        verifier.setPublicKey(N, E);
        verifier.setExpectedClaims(ISSUER, AUDIENCE, IMAGE_DIGEST);
        vm.stopPrank();
    }

    /// @notice A genuine RS256 signature over the exact signing input, with claims
    ///         present in the payload, is accepted and registers the signer.
    function test_ValidAttestationAccepted() public {
        vm.expectEmit(true, false, false, true, address(verifier));
        emit TeeAttestationVerifier.AttestationAccepted(ENCLAVE_SIGNER, EXP);

        verifier.submitAttestation(SIGNING_INPUT, PAYLOAD, SIG, ENCLAVE_SIGNER, EXP);

        assertEq(verifier.authorizedUntil(ENCLAVE_SIGNER), EXP);
    }

    /// @notice Flipping a single byte of a valid signature breaks the PKCS#1
    ///         padding structure or the recovered digest — reverts either way.
    function test_TamperedSignatureReverts() public {
        bytes memory tampered = SIG;
        tampered[0] = tampered[0] ^ 0x01;

        vm.expectRevert();
        verifier.submitAttestation(SIGNING_INPUT, PAYLOAD, tampered, ENCLAVE_SIGNER, EXP);
    }

    /// @notice A valid signature over a DIFFERENT signing input (even one byte
    ///         different) fails the digest comparison — the signature is bound to
    ///         the exact signed bytes, not just "some valid signature exists."
    function test_MismatchedSigningInputReverts() public {
        bytes memory wrongInput = SIGNING_INPUT;
        wrongInput[wrongInput.length - 1] = "X";

        vm.expectRevert(TeeAttestationVerifier.DigestMismatch.selector);
        verifier.submitAttestation(wrongInput, PAYLOAD, SIG, ENCLAVE_SIGNER, EXP);
    }

    function test_ExpiredTokenReverts() public {
        vm.expectRevert(abi.encodeWithSelector(TeeAttestationVerifier.TokenExpired.selector, EXP, EXP + 1));
        vm.warp(EXP + 1);
        verifier.submitAttestation(SIGNING_INPUT, PAYLOAD, SIG, ENCLAVE_SIGNER, EXP);
    }

    function test_WrongEnclaveSignerReverts() public {
        address wrongSigner = makeAddr("wrongSigner");
        vm.expectRevert(TeeAttestationVerifier.MissingSignerClaim.selector);
        verifier.submitAttestation(SIGNING_INPUT, PAYLOAD, SIG, wrongSigner, EXP);
    }

    function test_WrongIssuerClaimReverts() public {
        vm.prank(admin);
        verifier.setExpectedClaims("https://not-the-real-issuer.example", AUDIENCE, IMAGE_DIGEST);

        vm.expectRevert(
            abi.encodeWithSelector(
                TeeAttestationVerifier.ClaimNotFound.selector, bytes("https://not-the-real-issuer.example")
            )
        );
        verifier.submitAttestation(SIGNING_INPUT, PAYLOAD, SIG, ENCLAVE_SIGNER, EXP);
    }

    function test_WrongImageDigestClaimReverts() public {
        vm.prank(admin);
        verifier.setExpectedClaims(
            ISSUER, AUDIENCE, "sha256:0000000000000000000000000000000000000000000000000000000000000000"
        );

        vm.expectRevert();
        verifier.submitAttestation(SIGNING_INPUT, PAYLOAD, SIG, ENCLAVE_SIGNER, EXP);
    }

    function test_ZeroAddressSignerReverts() public {
        vm.expectRevert(TeeAttestationVerifier.ZeroAddress.selector);
        verifier.submitAttestation(SIGNING_INPUT, PAYLOAD, SIG, address(0), EXP);
    }

    function test_PublicKeyNotSetReverts() public {
        TeeAttestationVerifier fresh = new TeeAttestationVerifier(admin);
        vm.prank(admin);
        fresh.setExpectedClaims(ISSUER, AUDIENCE, IMAGE_DIGEST);

        vm.expectRevert(TeeAttestationVerifier.PublicKeyNotSet.selector);
        fresh.submitAttestation(SIGNING_INPUT, PAYLOAD, SIG, ENCLAVE_SIGNER, EXP);
    }

    function test_WrongSignatureLengthReverts() public {
        bytes memory shortSig = hex"aabbcc";
        vm.expectRevert(TeeAttestationVerifier.InvalidSignatureLength.selector);
        verifier.submitAttestation(SIGNING_INPUT, PAYLOAD, shortSig, ENCLAVE_SIGNER, EXP);
    }

    function test_NonAdminCannotSetPublicKey() public {
        vm.expectRevert();
        verifier.setPublicKey(N, E);
    }

    function test_NonAdminCannotSetExpectedClaims() public {
        vm.expectRevert();
        verifier.setExpectedClaims(ISSUER, AUDIENCE, IMAGE_DIGEST);
    }
}
