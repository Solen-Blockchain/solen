//! SRC-721: Solen NFT Standard
//!
//! A simple non-fungible token contract for the Solen network.
//!
//! ## Storage Layout
//!
//! | Key | Value | Description |
//! |-----|-------|-------------|
//! | `owner` | `[u8; 32]` | Contract owner (minter) |
//! | `name` | `bytes` | Collection name |
//! | `symbol` | `bytes` | Collection symbol |
//! | `next_id` | `u64` | Next token ID to mint |
//! | `tok/{id}` | `[u8; 32]` | Owner of token ID |
//! | `bal/{account}` | `u64` | Number of NFTs owned |
//!
//! ## Methods
//!
//! | Method | Args | Description |
//! |--------|------|-------------|
//! | `abi` | — | Returns JSON array of all methods |
//! | `init` | `name_len[1]+name[]+symbol_len[1]+symbol[]` | Initialize collection |
//! | `mint` | `to[32]` | Mint next NFT to address (owner only) |
//! | `transfer` | `to[32]+token_id[8]` | Transfer NFT |
//! | `owner_of` | `token_id[8]` | Query owner of NFT |
//! | `balance_of` | `account[32]` | Query NFT count for account |
//! | `total_supply` | — | Total minted NFTs |
//! | `name` | — | Collection name |
//! | `symbol` | — | Collection symbol |
//! | `owner` | — | Contract owner |

#![no_std]

use solen_contract_sdk::{events, sdk, storage};

fn token_key(id: u64) -> [u8; 12] {
    let mut key = [0u8; 12];
    key[..4].copy_from_slice(b"tok/");
    key[4..].copy_from_slice(&id.to_le_bytes());
    key
}

fn balance_key(account: &[u8; 32]) -> [u8; 36] {
    let mut key = [0u8; 36];
    key[..4].copy_from_slice(b"bal/");
    key[4..].copy_from_slice(account);
    key
}

fn get_balance(account: &[u8; 32]) -> u64 {
    storage::get_u64(&balance_key(account)).unwrap_or(0)
}

fn set_balance(account: &[u8; 32], val: u64) {
    storage::set_u64(&balance_key(account), val);
}

fn get_owner() -> [u8; 32] {
    let mut owner = [0u8; 32];
    if let Some(data) = storage::get(b"owner") {
        if data.len() >= 32 {
            owner.copy_from_slice(&data[..32]);
        }
    }
    owner
}

fn read_account(args: &[u8], offset: usize) -> Option<[u8; 32]> {
    if args.len() < offset + 32 { return None; }
    let mut a = [0u8; 32];
    a.copy_from_slice(&args[offset..offset + 32]);
    Some(a)
}

fn read_u64(args: &[u8], offset: usize) -> Option<u64> {
    if args.len() < offset + 8 { return None; }
    let mut buf = [0u8; 8];
    buf.copy_from_slice(&args[offset..offset + 8]);
    Some(u64::from_le_bytes(buf))
}

#[no_mangle]
pub extern "C" fn call(input_ptr: i32, input_len: i32) -> i32 {
    let input = sdk::read_input(input_ptr, input_len);
    let null_pos = input.iter().position(|&b| b == 0).unwrap_or(input.len());
    let method = &input[..null_pos];
    let args = if null_pos + 1 < input.len() { &input[null_pos + 1..] } else { &[] };

    match method {
        b"abi" => sdk::return_value(br#"[
{"name":"init","args":"name_len[1]+name[]+symbol_len[1]+symbol[]","mutates":true},
{"name":"mint","args":"to[32]","mutates":true},
{"name":"transfer","args":"to[32]+token_id[8]","mutates":true},
{"name":"owner_of","args":"token_id[8]","mutates":false},
{"name":"balance_of","args":"account[32]","mutates":false},
{"name":"total_supply","args":"","mutates":false},
{"name":"name","args":"","mutates":false},
{"name":"symbol","args":"","mutates":false},
{"name":"owner","args":"","mutates":false}
]"#),
        b"init" => {
            let caller = sdk::caller();
            storage::set(b"owner", &caller);
            storage::set_u64(b"next_id", 1);
            if args.len() >= 2 {
                let name_len = args[0] as usize;
                if args.len() >= 1 + name_len + 1 {
                    storage::set(b"name", &args[1..1 + name_len]);
                    let sym_start = 1 + name_len;
                    let sym_len = args[sym_start] as usize;
                    if args.len() >= sym_start + 1 + sym_len {
                        storage::set(b"symbol", &args[sym_start + 1..sym_start + 1 + sym_len]);
                    }
                }
            }
            events::emit(b"initialized", &caller);
            sdk::return_value(b"ok")
        }
        b"mint" => {
            let caller = sdk::caller();
            if caller != get_owner() {
                return sdk::return_value(b"err:unauthorized");
            }
            let to = match read_account(args, 0) {
                Some(a) => a,
                None => return sdk::return_value(b"err:invalid_args"),
            };
            let token_id = storage::get_u64(b"next_id").unwrap_or(1);
            storage::set(&token_key(token_id), &to);
            storage::set_u64(b"next_id", token_id + 1);
            set_balance(&to, get_balance(&to) + 1);
            events::emit(b"mint", &token_id.to_le_bytes());
            sdk::return_value(&token_id.to_le_bytes())
        }
        b"transfer" => {
            let caller = sdk::caller();
            let to = match read_account(args, 0) {
                Some(a) => a,
                None => return sdk::return_value(b"err:invalid_args"),
            };
            let token_id = match read_u64(args, 32) {
                Some(id) => id,
                None => return sdk::return_value(b"err:invalid_args"),
            };
            let key = token_key(token_id);
            let current_owner = match storage::get(&key) {
                Some(data) if data.len() >= 32 => {
                    let mut o = [0u8; 32];
                    o.copy_from_slice(&data[..32]);
                    o
                }
                _ => return sdk::return_value(b"err:token_not_found"),
            };
            if current_owner != caller {
                return sdk::return_value(b"err:not_owner");
            }
            storage::set(&key, &to);
            set_balance(&caller, get_balance(&caller).saturating_sub(1));
            set_balance(&to, get_balance(&to) + 1);
            events::emit(b"transfer", &token_id.to_le_bytes());
            sdk::return_value(b"ok")
        }
        b"owner_of" => {
            let token_id = match read_u64(args, 0) {
                Some(id) => id,
                None => return sdk::return_value(b"err:invalid_args"),
            };
            match storage::get(&token_key(token_id)) {
                Some(data) => sdk::return_value(data),
                None => sdk::return_value(b"err:token_not_found"),
            }
        }
        b"balance_of" => {
            let account = match read_account(args, 0) {
                Some(a) => a,
                None => return sdk::return_value(b"err:invalid_args"),
            };
            sdk::return_value(&get_balance(&account).to_le_bytes())
        }
        b"total_supply" => {
            let next_id = storage::get_u64(b"next_id").unwrap_or(1);
            let supply = next_id.saturating_sub(1);
            // Return as u128 for compatibility with SRC-20 detection.
            sdk::return_value(&(supply as u128).to_le_bytes())
        }
        b"name" => match storage::get(b"name") {
            Some(n) => sdk::return_value(n),
            None => sdk::return_value(b""),
        },
        b"symbol" => match storage::get(b"symbol") {
            Some(s) => sdk::return_value(s),
            None => sdk::return_value(b""),
        },
        b"owner" => sdk::return_value(&get_owner()),
        _ => sdk::return_value(b"unknown method"),
    }
}
