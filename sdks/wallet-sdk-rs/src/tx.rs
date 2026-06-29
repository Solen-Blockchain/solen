//! Transaction building and signing helpers.

use solen_crypto::{Keypair, MlDsaKeypair};
use solen_types::transaction::{Action, UserOperation};
use solen_types::AccountId;

/// Build an unsigned transfer operation.
pub fn build_transfer(sender: AccountId, nonce: u64, to: AccountId, amount: u128) -> UserOperation {
    UserOperation {
        sender,
        nonce,
        actions: vec![Action::Transfer { to, amount }],
        max_fee: 10_000,
        signature: vec![],
    }
}

/// Build an unsigned contract call operation.
pub fn build_call(
    sender: AccountId,
    nonce: u64,
    target: AccountId,
    method: &str,
    args: Vec<u8>,
) -> UserOperation {
    UserOperation {
        sender,
        nonce,
        actions: vec![Action::Call {
            target,
            method: method.to_string(),
            args,
        }],
        max_fee: 50_000,
        signature: vec![],
    }
}

/// Build an unsigned deploy operation.
pub fn build_deploy(
    sender: AccountId,
    nonce: u64,
    code: Vec<u8>,
    salt: [u8; 32],
) -> UserOperation {
    UserOperation {
        sender,
        nonce,
        actions: vec![Action::Deploy { code, salt }],
        max_fee: 100_000,
        signature: vec![],
    }
}

/// Compute the signing message for a user operation.
/// Thin wrapper around `UserOperation::signing_message` — kept for SDK
/// ergonomics so callers can do `signing_message(&op, chain_id)`.
pub fn signing_message(op: &UserOperation, chain_id: u64) -> Vec<u8> {
    op.signing_message(chain_id)
}

/// Sign a user operation in place (classical Ed25519).
pub fn sign_operation(op: &mut UserOperation, keypair: &Keypair, chain_id: u64) {
    let msg = signing_message(op, chain_id);
    let sig = keypair.sign(&msg);
    op.signature = sig.to_vec();
}

/// Sign a user operation in place with a post-quantum ML-DSA-65 key.
///
/// The account must be authorized by an `AuthMethod::MlDsa` whose public key
/// matches `keypair`, and the network must have post-quantum auth active
/// (`pq_auth_height`). Produces a ~3.3 KB signature.
pub fn sign_operation_ml_dsa(op: &mut UserOperation, keypair: &MlDsaKeypair, chain_id: u64) {
    let msg = signing_message(op, chain_id);
    op.signature = keypair.sign(&msg);
}

/// Build an unsigned `SetAuth` that rotates an account to post-quantum
/// (ML-DSA-65) authentication. Submit it signed by the account's CURRENT key
/// (e.g. via [`sign_operation`]); afterwards the account is authorized only by
/// the given ML-DSA public key. The account address is unchanged.
pub fn build_quantum_upgrade(
    sender: AccountId,
    nonce: u64,
    ml_dsa_public_key: Vec<u8>,
) -> UserOperation {
    use solen_types::account::AuthMethod;
    UserOperation {
        sender,
        nonce,
        actions: vec![Action::SetAuth {
            auth_methods: vec![AuthMethod::MlDsa {
                public_key: ml_dsa_public_key,
            }],
        }],
        max_fee: 1_000_000,
        signature: vec![],
    }
}

/// Sign an operation in place with an AND-hybrid keypair (Ed25519 + ML-DSA-65).
/// Both keys typically derive from the same 32-byte seed. The signature is
/// `ed25519[64] ‖ ml_dsa` — the layout the node's `Hybrid` auth method expects.
pub fn sign_operation_hybrid(
    op: &mut UserOperation,
    ed: &Keypair,
    ml: &MlDsaKeypair,
    chain_id: u64,
) {
    let msg = signing_message(op, chain_id);
    let mut sig = ed.sign(&msg).to_vec();
    sig.extend_from_slice(&ml.sign(&msg));
    op.signature = sig;
}

/// Build an unsigned `SetAuth` rotating an account to AND-hybrid auth (a
/// signature must then carry BOTH a valid Ed25519 and ML-DSA-65 signature).
pub fn build_hybrid_upgrade(
    sender: AccountId,
    nonce: u64,
    ed25519_public_key: [u8; 32],
    ml_dsa_public_key: Vec<u8>,
) -> UserOperation {
    use solen_types::account::AuthMethod;
    UserOperation {
        sender,
        nonce,
        actions: vec![Action::SetAuth {
            auth_methods: vec![AuthMethod::Hybrid {
                ed25519_public_key,
                ml_dsa_public_key,
            }],
        }],
        max_fee: 1_000_000,
        signature: vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn aid(n: u8) -> AccountId {
        let mut id = [0u8; 32];
        id[0] = n;
        id
    }

    #[test]
    fn build_and_sign_transfer() {
        let kp = Keypair::generate();
        let mut op = build_transfer(aid(1), 0, aid(2), 500);
        sign_operation(&mut op, &kp, 1337);

        assert_eq!(op.signature.len(), 64);

        // Verify the signature.
        let msg = signing_message(&op, 1337);
        let mut sig = [0u8; 64];
        sig.copy_from_slice(&op.signature);
        assert!(solen_crypto::verify(&kp.public_key(), &msg, &sig).is_ok());
    }

    #[test]
    fn build_and_sign_transfer_ml_dsa() {
        let kp = MlDsaKeypair::generate();
        let mut op = build_transfer(aid(1), 0, aid(2), 500);
        sign_operation_ml_dsa(&mut op, &kp, 1337);

        assert_eq!(op.signature.len(), solen_crypto::ML_DSA_SIG_LEN);
        let msg = signing_message(&op, 1337);
        assert!(solen_crypto::verify_ml_dsa(&kp.public_key(), &msg, &op.signature).is_ok());
    }

    #[test]
    fn quantum_upgrade_builds_setauth() {
        use solen_types::transaction::Action;
        let kp = MlDsaKeypair::generate();
        let op = build_quantum_upgrade(aid(1), 3, kp.public_key());
        assert!(matches!(op.actions[0], Action::SetAuth { .. }));
    }

    #[test]
    fn hybrid_sign_produces_both_signatures_in_order() {
        // One seed derives both keys; the signature is ed25519[64] ‖ ml_dsa, and
        // each half verifies (exactly what the node's Hybrid auth method checks).
        let seed = [4u8; 32];
        let ed = Keypair::from_seed(&seed);
        let ml = MlDsaKeypair::from_seed(&seed);
        let mut op = build_transfer(aid(1), 0, aid(2), 7);
        sign_operation_hybrid(&mut op, &ed, &ml, 1337);
        assert_eq!(op.signature.len(), 64 + solen_crypto::ML_DSA_SIG_LEN);
        let msg = signing_message(&op, 1337);
        let mut ed_sig = [0u8; 64];
        ed_sig.copy_from_slice(&op.signature[..64]);
        assert!(solen_crypto::verify(&ed.public_key(), &msg, &ed_sig).is_ok());
        assert!(solen_crypto::verify_ml_dsa(&ml.public_key(), &msg, &op.signature[64..]).is_ok());
    }

    #[test]
    fn build_call_and_deploy() {
        let call = build_call(aid(1), 0, aid(10), "increment", vec![]);
        assert_eq!(call.actions.len(), 1);

        let deploy = build_deploy(aid(1), 0, vec![0, 1, 2], [42u8; 32]);
        assert_eq!(deploy.actions.len(), 1);
    }
}
