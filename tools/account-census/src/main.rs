//! Read-only census of on-chain account auth-method types.
//!
//! Decodes the CURRENTLY DEPLOYED mainnet borsh layout of `Account` /
//! `AuthMethod` — deliberately self-contained, so it is unaffected by in-flight
//! changes to `solen-types`. Run it read-only against a live node's RocksDB to
//! confirm how many accounts use layout-sensitive auth methods (Passkey /
//! Session) before shipping a change to those variants.
//!
//! `open_for_read_only` does NOT take the DB lock, so this coexists safely with
//! the running node (it sees state as of the last flush, which is all we need).
//!
//! Usage: account-census <path-to-node-rocksdb-data-dir>

use borsh::{BorshDeserialize, BorshSerialize};
use rocksdb::{IteratorMode, Options, DB};

type AccountId = [u8; 32];

/// AuthMethod exactly as serialized by the deployed mainnet binary.
/// Variant order fixes the borsh discriminants — do not reorder.
#[derive(BorshSerialize, BorshDeserialize)]
enum AuthMethod {
    Passkey {
        credential_id: Vec<u8>,
        public_key_x: [u8; 32],
        public_key_y: [u8; 32],
    },
    Ed25519 {
        public_key: [u8; 32],
    },
    Threshold {
        signers: Vec<[u8; 32]>,
        threshold: u16,
    },
    Guardian {
        guardian_id: AccountId,
    },
    Session {
        session_key: [u8; 32],
        expires_at: u64,
        spending_limit: u128,
        allowed_targets: Vec<AccountId>,
        allowed_methods: Vec<String>,
    },
}

#[derive(BorshSerialize, BorshDeserialize)]
struct Account {
    id: AccountId,
    code_hash: [u8; 32],
    auth_methods: Vec<AuthMethod>,
    nonce: u64,
    balance: u128,
}

#[derive(Default)]
struct Census {
    accounts: u64,
    ed25519: u64,
    threshold: u64,
    guardian: u64,
    passkey: u64,
    session: u64,
    decode_errors: u64,
    passkey_ids: Vec<String>,
    session_ids: Vec<String>,
}

/// Account keys are `acc/` + 32-byte id (exactly 36 bytes). Everything else in
/// the store (blocks, chain meta, contract storage, session_spent/, slash/, …)
/// is ignored.
fn census<I: Iterator<Item = (Box<[u8]>, Box<[u8]>)>>(iter: I) -> Census {
    let mut c = Census::default();
    for (k, v) in iter {
        if !k.starts_with(b"acc/") || k.len() != 4 + 32 {
            continue;
        }
        match Account::try_from_slice(&v) {
            Ok(acct) => {
                c.accounts += 1;
                for m in &acct.auth_methods {
                    match m {
                        AuthMethod::Passkey { .. } => {
                            c.passkey += 1;
                            c.passkey_ids.push(hex::encode(acct.id));
                        }
                        AuthMethod::Ed25519 { .. } => c.ed25519 += 1,
                        AuthMethod::Threshold { .. } => c.threshold += 1,
                        AuthMethod::Guardian { .. } => c.guardian += 1,
                        AuthMethod::Session { .. } => {
                            c.session += 1;
                            c.session_ids.push(hex::encode(acct.id));
                        }
                    }
                }
            }
            Err(_) => c.decode_errors += 1,
        }
    }
    c
}

fn main() {
    let path = match std::env::args().nth(1) {
        Some(p) => p,
        None => {
            eprintln!("usage: account-census <path-to-node-rocksdb-data-dir>");
            std::process::exit(2);
        }
    };

    let db = match DB::open_for_read_only(&Options::default(), &path, false) {
        Ok(db) => db,
        Err(e) => {
            eprintln!("failed to open {path} read-only: {e}");
            std::process::exit(1);
        }
    };

    let iter = db
        .iterator(IteratorMode::Start)
        .filter_map(|r| r.ok());
    let c = census(iter);

    println!("accounts scanned : {}", c.accounts);
    println!("  ed25519        : {}", c.ed25519);
    println!("  threshold      : {}", c.threshold);
    println!("  guardian       : {}", c.guardian);
    println!("  passkey        : {}   <- borsh-layout-sensitive", c.passkey);
    println!("  session        : {}   <- borsh-layout-sensitive", c.session);
    if c.decode_errors > 0 {
        println!("  DECODE ERRORS  : {}  (investigate before deploying!)", c.decode_errors);
    }
    for id in &c.passkey_ids {
        println!("  passkey acct   : {id}");
    }
    for id in &c.session_ids {
        println!("  session acct   : {id}");
    }
    println!();
    if c.passkey == 0 && c.session == 0 && c.decode_errors == 0 {
        println!("RESULT: 0 passkey + 0 session accounts — SAFE to ship the AuthMethod layout change.");
    } else {
        println!("RESULT: layout-sensitive accounts exist (or decode errors) — migrate before deploying.");
        std::process::exit(3);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn acct(id: u8, methods: Vec<AuthMethod>) -> (Vec<u8>, Vec<u8>) {
        let mut aid = [0u8; 32];
        aid[0] = id;
        let a = Account {
            id: aid,
            code_hash: [0u8; 32],
            auth_methods: methods,
            nonce: 0,
            balance: 0,
        };
        let mut key = b"acc/".to_vec();
        key.extend_from_slice(&aid);
        (key, borsh::to_vec(&a).unwrap())
    }

    #[test]
    fn census_counts_layout_sensitive_methods_and_ignores_other_keys() {
        let dir = tempfile::tempdir().unwrap();
        {
            let db = DB::open_default(dir.path()).unwrap();
            let (k, v) = acct(1, vec![AuthMethod::Ed25519 { public_key: [1; 32] }]);
            db.put(k, v).unwrap();
            let (k, v) = acct(
                2,
                vec![
                    AuthMethod::Ed25519 { public_key: [2; 32] },
                    AuthMethod::Session {
                        session_key: [9; 32],
                        expires_at: 100,
                        spending_limit: 5,
                        allowed_targets: vec![],
                        allowed_methods: vec![],
                    },
                ],
            );
            db.put(k, v).unwrap();
            let (k, v) = acct(
                3,
                vec![AuthMethod::Passkey {
                    credential_id: vec![1, 2, 3],
                    public_key_x: [3; 32],
                    public_key_y: [4; 32],
                }],
            );
            db.put(k, v).unwrap();
            // Non-account keys that must be ignored.
            db.put(b"block/1", b"junk").unwrap();
            db.put(b"session_spent/abc", b"junk").unwrap();
            db.put(b"__chain_meta__", b"junk").unwrap();
        }

        let db = DB::open_for_read_only(&Options::default(), dir.path(), false).unwrap();
        let c = census(db.iterator(IteratorMode::Start).filter_map(|r| r.ok()));

        assert_eq!(c.accounts, 3, "three acc/ accounts");
        assert_eq!(c.ed25519, 2);
        assert_eq!(c.session, 1);
        assert_eq!(c.passkey, 1);
        assert_eq!(c.guardian, 0);
        assert_eq!(c.threshold, 0);
        assert_eq!(c.decode_errors, 0, "junk keys are skipped, not decode-failed");
        assert_eq!(c.session_ids.len(), 1);
        assert_eq!(c.passkey_ids.len(), 1);
    }
}
