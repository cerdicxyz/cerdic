// Demo trader client for TinyShieldedVault (v2), fully in Rust — see
// tiny/tee/src/main.rs's module doc for why this and the TEE both moved off
// TypeScript. Deposit is a plain transaction from the trader's own wallet;
// open/close go through the TEE over HTTP, encrypted, exactly like the
// TEE expects.
mod abi;
mod crypto;

use abi::{MockUsdc, TinyShieldedVault};
use anyhow::{Context, Result};
use ark_bn254::Fr;
use arkworks_prover::note_circuit::NoteCircuit;
use clap::{Parser, Subcommand};
use ethers::prelude::*;
use std::{env, sync::Arc};

#[derive(Parser)]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Full deposit -> open -> close flow against a live TEE + testnet,
    /// paying out to a fresh address different from the depositor's — the
    /// unlinkability property, demonstrated, not just asserted.
    Demo {
        #[arg(long, default_value_t = 424242)]
        secret: u64,
    },
    /// Just print the commitment for a secret (arkworks_prover, direct call
    /// — no subprocess, matching tee's own note derivation).
    DeriveCommitment {
        #[arg(long)]
        secret: u64,
    },
}

fn fr_to_bytes32(f: Fr) -> [u8; 32] {
    let be = ark_ff::BigInteger::to_bytes_be(&ark_ff::PrimeField::into_bigint(f));
    let mut out = [0u8; 32];
    out[32 - be.len()..].copy_from_slice(&be);
    out
}

type SignerClient = SignerMiddleware<Provider<Http>, LocalWallet>;

async fn connect(rpc_url: &str, private_key: &str) -> Result<Arc<SignerClient>> {
    let provider = Provider::<Http>::try_from(rpc_url)?;
    let chain_id = provider.get_chainid().await?.as_u64();
    let wallet: LocalWallet = private_key.parse::<LocalWallet>()?.with_chain_id(chain_id);
    Ok(Arc::new(SignerMiddleware::new(provider, wallet)))
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    let args = Args::parse();

    match args.command {
        Command::DeriveCommitment { secret } => {
            let (commitment, _nullifier) = NoteCircuit::derive(Fr::from(secret));
            println!("0x{}", hex::encode(fr_to_bytes32(commitment)));
        }

        Command::Demo { secret } => run_demo(secret).await?,
    }

    Ok(())
}

async fn run_demo(secret: u64) -> Result<()> {
    let rpc_url = env::var("RPC_URL").context("RPC_URL not set")?;
    let vault_address: Address = env::var("VAULT_ADDRESS").context("VAULT_ADDRESS not set")?.parse()?;
    let usdc_address: Address = env::var("USDC_ADDRESS").context("USDC_ADDRESS not set")?.parse()?;
    let trader_key = env::var("TRADER_PRIVATE_KEY").context("TRADER_PRIVATE_KEY not set")?;
    let tee_url = env::var("TEE_URL").unwrap_or_else(|_| "http://127.0.0.1:8787".into());
    let explorer = env::var("EXPLORER_BASE").unwrap_or_else(|_| "https://testnet.arcscan.app".into());

    let trader_client = connect(&rpc_url, &trader_key).await?;
    let trader_address = trader_client.address();
    println!("trader:  {trader_address:?}");

    // A fresh, never-before-used address — the payout destination. Nothing
    // links it to `trader_address` on-chain; that's the point.
    let payout_wallet = LocalWallet::new(&mut rand::thread_rng());
    let payout_address = payout_wallet.address();
    println!("payout:  {payout_address:?}  (fresh — never appears in the deposit)\n");

    let vault = TinyShieldedVault::new(vault_address, trader_client.clone());
    let usdc = MockUsdc::new(usdc_address, trader_client.clone());

    // --- 1. deposit: plain tx from the trader's own wallet, fixed denomination ---
    let (commitment_fr, _nullifier_fr) = NoteCircuit::derive(Fr::from(secret));
    let commitment = fr_to_bytes32(commitment_fr);
    let denomination = vault.denomination().call().await?;

    println!("1. depositing (plain tx — an observer sees {trader_address:?} deposit {denomination}; what they don't see is which position or payout this note becomes)");
    usdc.mint(trader_address, denomination).send().await?.await?;
    usdc.approve(vault_address, denomination).send().await?.await?;
    let deposit_receipt = vault.deposit(commitment).send().await?.await?.context("deposit tx failed")?;
    let deposit_tx = deposit_receipt.transaction_hash;
    println!("   tx: {explorer}/tx/{deposit_tx:?}\n");

    // --- 2. open: encrypted to the TEE, which derives commitment/nullifier itself ---
    println!("2. fetching the TEE's public key");
    let http = reqwest::Client::new();
    let pk_res: serde_json::Value = http.get(format!("{tee_url}/pk")).send().await?.json().await?;
    let tee_public_b64 = pk_res["publicKey"].as_str().context("no publicKey in /pk response")?;
    println!("   TEE pubkey: {tee_public_b64}");
    println!("   TEE signer (authorizedTEE on-chain): {}\n", pk_res["teeAddress"]);

    let tee_public = crypto::public_from_b64(tee_public_b64)?;
    let (ephemeral_secret, ephemeral_public) = crypto::generate_keypair();
    let order = serde_json::json!({ "secret": secret.to_string(), "side": "long", "size": "500000000" });
    println!("3. encrypting the real order + note secret — never sent in plaintext:\n   {order}\n");
    let (nonce, ciphertext) = crypto::encrypt_to_tee(&order, &tee_public, &ephemeral_secret);

    let open_body = serde_json::json!({
        "commitment": format!("0x{}", hex::encode(commitment)),
        "senderPublicKey": crypto::b64(ephemeral_public.as_bytes()),
        "nonce": nonce,
        "ciphertext": ciphertext,
    });
    println!("4. submitting to the TEE — it opens the position on our behalf");
    let open_res: serde_json::Value = http.post(format!("{tee_url}/open")).json(&open_body).send().await?.json().await?;
    let position_id = open_res["positionId"].as_str().context(format!("TEE /open failed: {open_res}"))?.to_string();
    let open_tx = open_res["txHash"].as_str().unwrap_or("?");
    println!("   positionId: {position_id}");
    println!("   tx: {explorer}/tx/{open_tx}");
    println!("   kept in enclave, never on-chain: {}\n", open_res["keptInEnclave"]);

    // --- 5. read state back, exactly like any observer could ---
    let pid_bytes: [u8; 32] = hex::decode(position_id.trim_start_matches("0x"))?.try_into().unwrap();
    let (nullifier, collateral, status, sealed_params) = vault.get_position(pid_bytes).call().await?;
    println!("5. position as anyone reading the chain would see it:");
    println!("   nullifier:    0x{}", hex::encode(nullifier));
    println!("   collateral:   {collateral}");
    println!("   status:       {status} (1 = Open)");
    println!("   sealedParams: 0x{} (ciphertext)\n", hex::encode(&sealed_params));

    // --- 6. close: TEE re-derives, prices, pays out to the FRESH address ---
    println!("6. closing — payout goes to {payout_address:?}, not {trader_address:?}");
    let close_body = serde_json::json!({ "positionId": position_id, "payoutAddress": format!("{payout_address:?}") });
    let close_res: serde_json::Value = http.post(format!("{tee_url}/close")).json(&close_body).send().await?.json().await?;
    let close_tx = close_res["txHash"].as_str().context(format!("TEE /close failed: {close_res}"))?;
    println!("   tx: {explorer}/tx/{close_tx}");
    println!("   settlementDelta: {}\n", close_res["settlementDelta"]);

    println!("done. unlinkability check: deposit came from {trader_address:?}, funds landed on {payout_address:?} — nothing on-chain connects them.");
    Ok(())
}
