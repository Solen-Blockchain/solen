//! Transaction building and signing helpers.

use solen_crypto::Keypair;
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

/// Sign a user operation in place.
pub fn sign_operation(op: &mut UserOperation, keypair: &Keypair, chain_id: u64) {
    let msg = signing_message(op, chain_id);
    let sig = keypair.sign(&msg);
    op.signature = sig.to_vec();
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
    fn build_call_and_deploy() {
        let call = build_call(aid(1), 0, aid(10), "increment", vec![]);
        assert_eq!(call.actions.len(), 1);

        let deploy = build_deploy(aid(1), 0, vec![0, 1, 2], [42u8; 32]);
        assert_eq!(deploy.actions.len(), 1);
    }
}
