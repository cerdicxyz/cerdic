// One-off: generates the two keypairs the TEE needs and prints them as
// .env-ready lines. Run once, save the output, then start tiny-tee with
// those values pinned (so restarts don't lose the ability to
// decrypt/close already-open positions).
use ethers::signers::{LocalWallet, Signer};

#[path = "../crypto.rs"]
mod crypto;

fn main() {
    let eth_wallet = LocalWallet::new(&mut rand::thread_rng());
    let (box_secret, box_public) = crypto::generate_box_keypair();

    println!("# --- TEE identity (Ethereum signer, calls TinyShieldedVault) ---");
    println!("TEE_ADDRESS={:?}", eth_wallet.address());
    println!("TEE_PRIVATE_KEY=0x{}", hex::encode(eth_wallet.signer().to_bytes()));
    println!();
    println!("# --- TEE encryption keypair (X25519, order decryption + param sealing) ---");
    println!("BOX_PUBLIC_KEY_B64={}", crypto::b64(box_public.as_bytes()));
    println!("BOX_SECRET_KEY_B64={}", crypto::b64(box_secret.to_bytes().as_slice()));
    println!();
    println!("# TEE_ADDRESS goes into tiny/contracts/.env as TEE_ADDRESS (Deploy2.s.sol).");
    println!("# All four values go into tiny/tee/.env.");
}
