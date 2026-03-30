//! SRC-20: Solen Token Standard
//!
//! An ERC20-equivalent fungible token contract for the Solen network.
//!
//! Supports:
//! - Token metadata (name, symbol, decimals)
//! - Minting (by the deployer/owner)
//! - Transfers between accounts
//! - Allowance-based approvals and transferFrom
//! - Balance and supply queries
//!
//! ## Storage Layout
//!
//! | Key | Value | Description |
//! |-----|-------|-------------|
//! | `owner` | `[u8; 32]` | Contract owner (deployer) |
//! | `total_supply` | `u128` | Total minted supply |
//! | `bal/{account}` | `u128` | Balance of an account |
//! | `allow/{owner}/{spender}` | `u128` | Spending allowance |
//!
//! ## Methods
//!
//! Input format: `method_name\0arg_bytes`
//!
//! | Method | Args | Description |
//! |--------|------|-------------|
//! | `abi` | — | Returns JSON array of all methods |
//! | `init` | `name_len[1]+name[]+symbol_len[1]+symbol[]` | Initialize with metadata |
//! | `mint` | `to[32] + amount[16]` | Mint tokens (owner only) |
//! | `transfer` | `to[32] + amount[16]` | Transfer tokens |
//! | `approve` | `spender[32] + amount[16]` | Set allowance |
//! | `transfer_from` | `from[32] + to[32] + amount[16]` | Transfer using allowance |
//! | `balance_of` | `account[32]` | Query balance (returns u128) |
//! | `allowance` | `owner[32] + spender[32]` | Query allowance (returns u128) |
//! | `total_supply` | — | Query total supply (returns u128) |
//! | `name` | — | Token name (UTF-8 string) |
//! | `symbol` | — | Token symbol/ticker (UTF-8 string) |
//! | `decimals` | — | Decimal places (1 byte, default 8) |

#![no_std]

use solen_contract_sdk::{events, sdk, storage};

// ── Storage key builders ────────────────────────────────────────

fn balance_key(account: &[u8; 32]) -> [u8; 36] {
    let mut key = [0u8; 36];
    key[..4].copy_from_slice(b"bal/");
    key[4..].copy_from_slice(account);
    key
}

fn allowance_key(owner: &[u8; 32], spender: &[u8; 32]) -> [u8; 70] {
    let mut key = [0u8; 70];
    key[..6].copy_from_slice(b"allow/");
    key[6..38].copy_from_slice(owner);
    key[38..39].copy_from_slice(b"/");
    key[39..71].copy_from_slice(spender);
    // truncate to 70
    key
}

// ── Storage helpers ─────────────────────────────────────────────

fn get_balance(account: &[u8; 32]) -> u128 {
    let key = balance_key(account);
    storage::get_u128(&key).unwrap_or(0)
}

fn set_balance(account: &[u8; 32], amount: u128) {
    let key = balance_key(account);
    storage::set_u128(&key, amount);
}

fn get_allowance(owner: &[u8; 32], spender: &[u8; 32]) -> u128 {
    let key = allowance_key(owner, spender);
    storage::get_u128(&key).unwrap_or(0)
}

fn set_allowance(owner: &[u8; 32], spender: &[u8; 32], amount: u128) {
    let key = allowance_key(owner, spender);
    storage::set_u128(&key, amount);
}

fn get_total_supply() -> u128 {
    storage::get_u128(b"total_supply").unwrap_or(0)
}

fn set_total_supply(supply: u128) {
    storage::set_u128(b"total_supply", supply);
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

fn set_owner(owner: &[u8; 32]) {
    storage::set(b"owner", owner);
}

// ── Argument parsing ────────────────────────────────────────────

fn read_account(args: &[u8], offset: usize) -> Option<[u8; 32]> {
    if args.len() < offset + 32 {
        return None;
    }
    let mut account = [0u8; 32];
    account.copy_from_slice(&args[offset..offset + 32]);
    Some(account)
}

fn read_u128(args: &[u8], offset: usize) -> Option<u128> {
    if args.len() < offset + 16 {
        return None;
    }
    let mut buf = [0u8; 16];
    buf.copy_from_slice(&args[offset..offset + 16]);
    Some(u128::from_le_bytes(buf))
}

// ── Entry point ─────────────────────────────────────────────────

#[no_mangle]
pub extern "C" fn call(input_ptr: i32, input_len: i32) -> i32 {
    let input = sdk::read_input(input_ptr, input_len);

    // Parse method name: everything before the first null byte.
    let null_pos = input.iter().position(|&b| b == 0).unwrap_or(input.len());
    let method = &input[..null_pos];
    let args = if null_pos + 1 < input.len() {
        &input[null_pos + 1..]
    } else {
        &[]
    };

    match method {
        b"abi" => do_abi(),
        b"init" => do_init(args),
        b"mint" => do_mint(args),
        b"transfer" => do_transfer(args),
        b"approve" => do_approve(args),
        b"transfer_from" => do_transfer_from(args),
        b"balance_of" => do_balance_of(args),
        b"allowance" => do_allowance(args),
        b"total_supply" => do_total_supply(),
        b"name" => do_name(),
        b"symbol" => do_symbol(),
        b"decimals" => do_decimals(),
        _ => sdk::return_value(b"unknown method"),
    }
}

// ── Method implementations ──────────────────────────────────────

fn do_abi() -> i32 {
    sdk::return_value(br#"[
{"name":"init","args":"name_len[1]+name[]+symbol_len[1]+symbol[]","mutates":true},
{"name":"mint","args":"to[32]+amount[16]","mutates":true},
{"name":"transfer","args":"to[32]+amount[16]","mutates":true},
{"name":"approve","args":"spender[32]+amount[16]","mutates":true},
{"name":"transfer_from","args":"from[32]+to[32]+amount[16]","mutates":true},
{"name":"balance_of","args":"account[32]","mutates":false},
{"name":"allowance","args":"owner[32]+spender[32]","mutates":false},
{"name":"total_supply","args":"","mutates":false},
{"name":"name","args":"","mutates":false},
{"name":"symbol","args":"","mutates":false},
{"name":"decimals","args":"","mutates":false}
]"#)
}

fn do_init(args: &[u8]) -> i32 {
    let caller = sdk::caller();
    set_owner(&caller);

    // Parse optional name and symbol from args: name_len[1] + name[N] + symbol_len[1] + symbol[M]
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

    // Default decimals to 8 (matching SOLEN native).
    storage::set(b"decimals", &[8]);

    events::emit(b"initialized", &caller);
    sdk::return_value(b"ok")
}

fn do_name() -> i32 {
    match storage::get(b"name") {
        Some(name) => sdk::return_value(name),
        None => sdk::return_value(b""),
    }
}

fn do_symbol() -> i32 {
    match storage::get(b"symbol") {
        Some(sym) => sdk::return_value(sym),
        None => sdk::return_value(b""),
    }
}

fn do_decimals() -> i32 {
    match storage::get(b"decimals") {
        Some(d) => sdk::return_value(d),
        None => sdk::return_value(&[8]),
    }
}

fn do_mint(args: &[u8]) -> i32 {
    let caller = sdk::caller();
    let owner = get_owner();

    // Only the owner can mint.
    if caller != owner {
        return sdk::return_value(b"err:unauthorized");
    }

    let to = match read_account(args, 0) {
        Some(a) => a,
        None => return sdk::return_value(b"err:invalid_args"),
    };
    let amount = match read_u128(args, 32) {
        Some(a) => a,
        None => return sdk::return_value(b"err:invalid_args"),
    };

    let balance = get_balance(&to);
    set_balance(&to, balance + amount);

    let supply = get_total_supply();
    set_total_supply(supply + amount);

    events::emit(b"mint", &amount.to_le_bytes());
    sdk::return_value(&(balance + amount).to_le_bytes())
}

fn do_transfer(args: &[u8]) -> i32 {
    let caller = sdk::caller();

    let to = match read_account(args, 0) {
        Some(a) => a,
        None => return sdk::return_value(b"err:invalid_args"),
    };
    let amount = match read_u128(args, 32) {
        Some(a) => a,
        None => return sdk::return_value(b"err:invalid_args"),
    };

    let from_balance = get_balance(&caller);
    if from_balance < amount {
        return sdk::return_value(b"err:insufficient_balance");
    }

    set_balance(&caller, from_balance - amount);
    let to_balance = get_balance(&to);
    set_balance(&to, to_balance + amount);

    events::emit(b"transfer", &amount.to_le_bytes());
    sdk::return_value(b"ok")
}

fn do_approve(args: &[u8]) -> i32 {
    let caller = sdk::caller();

    let spender = match read_account(args, 0) {
        Some(a) => a,
        None => return sdk::return_value(b"err:invalid_args"),
    };
    let amount = match read_u128(args, 32) {
        Some(a) => a,
        None => return sdk::return_value(b"err:invalid_args"),
    };

    set_allowance(&caller, &spender, amount);

    events::emit(b"approval", &amount.to_le_bytes());
    sdk::return_value(b"ok")
}

fn do_transfer_from(args: &[u8]) -> i32 {
    let caller = sdk::caller();

    let from = match read_account(args, 0) {
        Some(a) => a,
        None => return sdk::return_value(b"err:invalid_args"),
    };
    let to = match read_account(args, 32) {
        Some(a) => a,
        None => return sdk::return_value(b"err:invalid_args"),
    };
    let amount = match read_u128(args, 64) {
        Some(a) => a,
        None => return sdk::return_value(b"err:invalid_args"),
    };

    // Check allowance.
    let allowed = get_allowance(&from, &caller);
    if allowed < amount {
        return sdk::return_value(b"err:insufficient_allowance");
    }

    // Check balance.
    let from_balance = get_balance(&from);
    if from_balance < amount {
        return sdk::return_value(b"err:insufficient_balance");
    }

    // Execute transfer.
    set_balance(&from, from_balance - amount);
    let to_balance = get_balance(&to);
    set_balance(&to, to_balance + amount);

    // Reduce allowance.
    set_allowance(&from, &caller, allowed - amount);

    events::emit(b"transfer", &amount.to_le_bytes());
    sdk::return_value(b"ok")
}

fn do_balance_of(args: &[u8]) -> i32 {
    let account = match read_account(args, 0) {
        Some(a) => a,
        None => return sdk::return_value(b"err:invalid_args"),
    };
    let balance = get_balance(&account);
    sdk::return_value(&balance.to_le_bytes())
}

fn do_allowance(args: &[u8]) -> i32 {
    let owner = match read_account(args, 0) {
        Some(a) => a,
        None => return sdk::return_value(b"err:invalid_args"),
    };
    let spender = match read_account(args, 32) {
        Some(a) => a,
        None => return sdk::return_value(b"err:invalid_args"),
    };
    let allowed = get_allowance(&owner, &spender);
    sdk::return_value(&allowed.to_le_bytes())
}

fn do_total_supply() -> i32 {
    let supply = get_total_supply();
    sdk::return_value(&supply.to_le_bytes())
}
