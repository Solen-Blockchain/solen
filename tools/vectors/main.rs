//! Cross-language test vectors for Solen transaction signing.
//!
//! Emits a JSON file of (chain_id, UserOperation) → signing_message_bytes →
//! signature pairs derived from deterministic test seeds. Any external signer
//! port (Trust Wallet Core, Ledger, hardware wallets, third-party SDKs) MUST
//! reproduce these byte-for-byte before being trusted on mainnet.
//!
//! Usage:
//!     solen-vectors generate --out vectors.json
//!     solen-vectors check    --in  vectors.json   # fails with non-zero exit if any vector mismatches

use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};
use solen_crypto::Keypair;
use solen_types::account::AuthMethod;
use solen_types::transaction::{Action, UserOperation};

#[derive(Parser)]
#[command(name = "solen-vectors", version, about = "Generate and check Solen signing test vectors")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    Generate {
        #[arg(long, default_value = "vectors.json")]
        out: PathBuf,
    },
    Check {
        #[arg(long, default_value = "vectors.json")]
        r#in: PathBuf,
    },
}

#[derive(Serialize, Deserialize)]
struct VectorFile {
    spec_version: u32,
    description: String,
    signer_seed_hex: String,
    cosigner_seed_hex: String,
    vectors: Vec<Vector>,
}

#[derive(Serialize, Deserialize)]
struct Vector {
    name: String,
    chain_id: u64,
    operation: OpJson,
    signing_message_hex: String,
    signature_hex: String,
}

#[derive(Serialize, Deserialize)]
struct OpJson {
    sender_hex: String,
    nonce: u64,
    /// Decimal string — u128 exceeds JSON safe-integer range in JS.
    max_fee_dec: String,
    /// The exact canonical JSON bytes of `actions` that get hashed into
    /// `actions_hash`. Stored as a UTF-8 string so cross-language porters
    /// can diff their serializer output byte-for-byte against this.
    actions_canonical_json: String,
    /// Convenience: BLAKE3(actions_canonical_json) hex-encoded.
    actions_hash_hex: String,
}

const SIGNER_SEED: [u8; 32] = [0x11; 32];
const COSIGNER_SEED: [u8; 32] = [0x22; 32];
const RECIPIENT_SEED: [u8; 32] = [0x33; 32];

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Generate { out } => {
            let file = build_vectors();
            let json = serde_json::to_string_pretty(&file).expect("serialize vectors");
            fs::write(&out, json).expect("write vectors file");
            eprintln!("wrote {} vectors to {}", file.vectors.len(), out.display());
            ExitCode::SUCCESS
        }
        Cmd::Check { r#in } => {
            let raw = fs::read_to_string(&r#in).expect("read vectors file");
            let recorded: VectorFile = serde_json::from_str(&raw).expect("parse vectors file");
            let regenerated = build_vectors();

            if recorded.vectors.len() != regenerated.vectors.len() {
                eprintln!(
                    "vector count mismatch: recorded={}, regenerated={}",
                    recorded.vectors.len(),
                    regenerated.vectors.len()
                );
                return ExitCode::FAILURE;
            }

            let mut failed = 0usize;
            for (rec, gen) in recorded.vectors.iter().zip(regenerated.vectors.iter()) {
                if rec.signing_message_hex != gen.signing_message_hex {
                    eprintln!(
                        "MISMATCH [{}]: signing_message\n  recorded:    {}\n  regenerated: {}",
                        rec.name, rec.signing_message_hex, gen.signing_message_hex
                    );
                    failed += 1;
                } else if rec.signature_hex != gen.signature_hex {
                    eprintln!(
                        "MISMATCH [{}]: signature\n  recorded:    {}\n  regenerated: {}",
                        rec.name, rec.signature_hex, gen.signature_hex
                    );
                    failed += 1;
                }
            }

            if failed == 0 {
                eprintln!("OK: all {} vectors match", recorded.vectors.len());
                ExitCode::SUCCESS
            } else {
                eprintln!("{} of {} vectors mismatch", failed, recorded.vectors.len());
                ExitCode::FAILURE
            }
        }
    }
}

fn build_vectors() -> VectorFile {
    let signer = Keypair::from_seed(&SIGNER_SEED);
    let cosigner = Keypair::from_seed(&COSIGNER_SEED);
    let recipient = Keypair::from_seed(&RECIPIENT_SEED);

    let sender_pub = signer.public_key();
    let cosigner_pub = cosigner.public_key();
    let recipient_pub = recipient.public_key();

    let mut vectors = Vec::new();

    // -- Transfer vectors --------------------------------------------------
    vectors.push(make_vector(
        "transfer_simple_mainnet",
        1,
        sender_pub,
        0,
        100_000,
        vec![Action::Transfer {
            to: recipient_pub,
            amount: 1_500_000_000,
        }],
        &signer,
    ));
    vectors.push(make_vector(
        "transfer_simple_testnet",
        9000,
        sender_pub,
        42,
        200,
        vec![Action::Transfer {
            to: recipient_pub,
            amount: 1,
        }],
        &signer,
    ));
    vectors.push(make_vector(
        "transfer_zero_amount",
        1,
        sender_pub,
        7,
        100,
        vec![Action::Transfer {
            to: recipient_pub,
            amount: 0,
        }],
        &signer,
    ));
    vectors.push(make_vector(
        "transfer_max_amount_u128",
        1,
        sender_pub,
        1,
        u128::MAX,
        vec![Action::Transfer {
            to: recipient_pub,
            amount: u128::MAX,
        }],
        &signer,
    ));
    vectors.push(make_vector(
        "transfer_nonce_zero",
        1,
        sender_pub,
        0,
        200,
        vec![Action::Transfer {
            to: recipient_pub,
            amount: 100,
        }],
        &signer,
    ));
    vectors.push(make_vector(
        "transfer_nonce_max_u64",
        1,
        sender_pub,
        u64::MAX,
        200,
        vec![Action::Transfer {
            to: recipient_pub,
            amount: 100,
        }],
        &signer,
    ));
    vectors.push(make_vector(
        "transfer_max_fee_zero",
        1,
        sender_pub,
        2,
        0,
        vec![Action::Transfer {
            to: recipient_pub,
            amount: 1,
        }],
        &signer,
    ));
    vectors.push(make_vector(
        "transfer_to_self",
        1,
        sender_pub,
        3,
        100,
        vec![Action::Transfer {
            to: sender_pub,
            amount: 1,
        }],
        &signer,
    ));
    vectors.push(make_vector(
        "transfer_devnet_chain_1337",
        1337,
        sender_pub,
        4,
        100,
        vec![Action::Transfer {
            to: recipient_pub,
            amount: 1,
        }],
        &signer,
    ));

    // -- Multi-action --------------------------------------------------------
    vectors.push(make_vector(
        "multi_action_two_transfers",
        1,
        sender_pub,
        5,
        500,
        vec![
            Action::Transfer {
                to: recipient_pub,
                amount: 100,
            },
            Action::Transfer {
                to: cosigner_pub,
                amount: 200,
            },
        ],
        &signer,
    ));

    // -- Call vectors --------------------------------------------------------
    vectors.push(make_vector(
        "call_empty_args",
        1,
        sender_pub,
        10,
        10_000,
        vec![Action::Call {
            target: recipient_pub,
            method: "ping".to_string(),
            args: vec![],
        }],
        &signer,
    ));
    vectors.push(make_vector(
        "call_with_args",
        1,
        sender_pub,
        11,
        10_000,
        vec![Action::Call {
            target: recipient_pub,
            method: "transfer".to_string(),
            args: vec![0xDE, 0xAD, 0xBE, 0xEF],
        }],
        &signer,
    ));

    // -- Deploy vector -------------------------------------------------------
    vectors.push(make_vector(
        "deploy_small_code",
        1,
        sender_pub,
        20,
        1_000_000,
        vec![Action::Deploy {
            code: vec![0x00, 0x61, 0x73, 0x6D, 0x01, 0x00, 0x00, 0x00], // wasm magic + version
            salt: [0xAA; 32],
        }],
        &signer,
    ));

    // -- SetAuth vectors -----------------------------------------------------
    vectors.push(make_vector(
        "setauth_ed25519",
        1,
        sender_pub,
        30,
        1_000,
        vec![Action::SetAuth {
            auth_methods: vec![AuthMethod::Ed25519 {
                public_key: cosigner_pub,
            }],
        }],
        &signer,
    ));
    vectors.push(make_vector(
        "setauth_threshold_2of3",
        1,
        sender_pub,
        31,
        1_000,
        vec![Action::SetAuth {
            auth_methods: vec![AuthMethod::Threshold {
                signers: vec![sender_pub, cosigner_pub, recipient_pub],
                threshold: 2,
            }],
        }],
        &signer,
    ));

    VectorFile {
        spec_version: 1,
        description: "Solen transaction signing test vectors. See \
                      docs.solenchain.io/specs/transaction-signing for the canonical \
                      byte-level spec. Regenerate with `cargo run -p solen-vectors -- generate`."
            .to_string(),
        signer_seed_hex: hex(&SIGNER_SEED),
        cosigner_seed_hex: hex(&COSIGNER_SEED),
        vectors,
    }
}

fn make_vector(
    name: &str,
    chain_id: u64,
    sender: [u8; 32],
    nonce: u64,
    max_fee: u128,
    actions: Vec<Action>,
    signer: &Keypair,
) -> Vector {
    let op = UserOperation {
        sender,
        nonce,
        actions,
        max_fee,
        signature: vec![],
    };
    let msg = op.signing_message(chain_id);
    let sig = signer.sign(&msg);
    let actions_bytes = serde_json::to_vec(&op.actions).expect("actions serialize");
    let actions_hash = solen_crypto::blake3_hash(&actions_bytes);

    Vector {
        name: name.to_string(),
        chain_id,
        operation: OpJson {
            sender_hex: hex(&sender),
            nonce,
            max_fee_dec: max_fee.to_string(),
            actions_canonical_json: String::from_utf8(actions_bytes).expect("utf-8 json"),
            actions_hash_hex: hex(&actions_hash),
        },
        signing_message_hex: hex(&msg),
        signature_hex: hex(&sig),
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}
