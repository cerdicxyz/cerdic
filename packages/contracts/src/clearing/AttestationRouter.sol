// SPDX-License-Identifier: MIT
pragma solidity 0.8.35;
import {AccessControl} from "openzeppelin-contracts/contracts/access/AccessControl.sol";

/// @title  AttestationRouter
/// @notice Allowlist of TEE addresses authorized to call SettlementEngine.settleMatch.
/// @dev    MVP scope: admin registers a TEE address after verifying its attestation
///         token off-chain. On-chain OIDC/COSE verification (TeeAttestationVerifier,
///         see clearing/TeeAttestationVerifier.sol) replaces this with a real check
///         later; the isAuthorizedTEE surface this router exposes doesn't need to
///         change when that lands.
contract AttestationRouter is AccessControl {
    bytes32 public constant ROUTER_ADMIN_ROLE = keccak256("ROUTER_ADMIN_ROLE");

    mapping(address => bool) public isAuthorizedTEE;

    event TEEAuthorized(address indexed tee);
    event TEERevoked(address indexed tee);

    error ZeroAddress();

    constructor(address admin) {
        if (admin == address(0)) revert ZeroAddress();
        _grantRole(DEFAULT_ADMIN_ROLE, admin);
        _grantRole(ROUTER_ADMIN_ROLE, admin);
    }

    function authorizeTEE(address tee) external onlyRole(ROUTER_ADMIN_ROLE) {
        if (tee == address(0)) revert ZeroAddress();
        isAuthorizedTEE[tee] = true;
        emit TEEAuthorized(tee);
    }

    function revokeTEE(address tee) external onlyRole(ROUTER_ADMIN_ROLE) {
        isAuthorizedTEE[tee] = false;
        emit TEERevoked(tee);
    }
}
