//! stSOLEN — Liquid Staking Derivative for Solen
//!
//! An SRC-20 receipt token representing a claim on staked SOLEN plus accrued
//! rewards. The contract holds delegated SOLEN under its own account, so
//! `delegator = self_id()` on the staking system contract. Rewards auto-credit
//! to the contract balance each epoch; the contract picks up the inflow via
//! `_sync_rewards()` on every state-changing call, raising the exchange rate.
//!
//! Spec: `Solen ecosystem → Tier-1 #1 (stSOLEN v1)`. Implementation depends on
//! the executor patch that routes queued contract→system-contract calls
//! (so this contract can `queue_call(STAKING_ADDRESS, "delegate", …)`).
//!
//! ## Storage layout
//!
//! | Key | Type | Description |
//! |---|---|---|
//! | `owner` | `[u8;32]` | Admin multisig |
//! | `treasury` | `[u8;32]` | Receives fee-minted stSOLEN |
//! | `slash_oracle` | `[u8;32]` | Authorized to call `report_slash` |
//! | `paused` | `u8` | 1 = halt deposits/withdrawals (transfers always allowed) |
//! | `total_supply` | `u128` | Total stSOLEN in circulation |
//! | `total_pooled_solen` | `u128` | SOLEN backing all stSOLEN; updated on delegate / un-realize / slash / sync |
//! | `last_balance_seen` | `u128` | Account balance at end of last `_sync_rewards` |
//! | `pending_withdrawal_solen` | `u128` | Sum of `solen_owed` across unclaimed `wq` entries |
//! | `protocol_fee_bps` | `u64` | Reward skim, default 1000 (10%), hard cap 2000 |
//! | `op_count` | `u64` | Length of operator allowlist |
//! | `op_cursor` | `u64` | Round-robin pointer |
//! | `op_cap_bps` | `u64` | Per-operator cap, default 2500 (25%) |
//! | `last_recompound_epoch` | `u64` | Rate-limit for `recompound_rewards` |
//! | `wq_head` | `u64` | Next withdrawal seq to serve |
//! | `wq_tail` | `u64` | Next withdrawal seq to assign |
//! | `bal/{account}` | `u128` | SRC-20 balance |
//! | `allow/{owner}/{spender}` | `u128` | SRC-20 allowance |
//! | `op/{i}` | `[u8;32]` | Operator at allowlist index `i` |
//! | `op_stake/{operator}` | `u128` | Tracked stake per operator |
//! | `pue/{operator}` | `u128` | Pending undelegate amount queued for `crank` |
//! | `ifu/{operator}` | `u64` | In-flight undelegations against operator (≤ 7) |
//! | `wq/{seq}` | 56 bytes | `account[32] ‖ solen_owed[16] ‖ requested_epoch[8]` |

#![no_std]
// Some constants (e.g. `SOLEN_BASE_UNIT`) are exported for external readers
// of the source; allow dead code here rather than peppering individual
// `#[allow]` attributes.
#![allow(dead_code)]

use solen_contract_sdk::{events, sdk, storage};

// ── Constants ────────────────────────────────────────────────────

const STAKING_ADDRESS: [u8; 32] = {
    let mut a = [0xFFu8; 32];
    a[31] = 0x01;
    a
};

/// Epoch length in blocks (matches `solen-consensus`).
const EPOCH_LENGTH_BLOCKS: u64 = 100;
/// Unbonding cooldown in epochs (matches staking `DEFAULT_UNBONDING_PERIOD`).
const UNBONDING_EPOCHS: u64 = 7;
/// Reserve the contract retains so the staking system call's
/// `balance >= amount + MIN_FEE_RESERVE` precondition is always met.
const MIN_FEE_RESERVE: u128 = 10_000;
/// Hard cap on the operator allowlist size.
const MAX_OPERATORS: u64 = 32;
/// Cap on in-flight undelegations per (delegator, validator) pair, mirroring
/// `solen-system-contracts/staking::MAX_UNDELEGATION_ENTRIES`. We always
/// leave a slot of headroom (`+1 >= cap`) so concurrent cranker invocations
/// don't self-block.
const MAX_UNDELEGATIONS_PER_OP: u64 = 7;
/// Permanent burn destination for the bootstrap-attack mitigation lockup.
const DEAD_ADDRESS: [u8; 32] = [0xDE; 32];
/// stSOLEN burned to dead on the first deposit.
const BOOTSTRAP_BURN: u128 = 1_000;
/// Minimum first deposit (base units). Must exceed `MIN_FEE_RESERVE +
/// BOOTSTRAP_BURN` so the contract can delegate something AND retain the
/// staking-call reserve. Spec §11 Q3 noted 1100 but didn't account for the
/// reserve — bumped accordingly.
const MIN_FIRST_DEPOSIT: u128 = 11_100;
/// Hard cap on `protocol_fee_bps`.
const FEE_BPS_HARD_CAP: u64 = 2_000;

// ── Defaults applied at `init` ───────────────────────────────────
const DEFAULT_FEE_BPS: u64 = 1_000;
const DEFAULT_OP_CAP_BPS: u64 = 2_500;

// ── Token metadata ───────────────────────────────────────────────
const NAME: &[u8] = b"Staked SOLEN";
const SYMBOL: &[u8] = b"stSOLEN";
const DECIMALS: u8 = 8;

// ── Storage key builders ─────────────────────────────────────────

fn balance_key(account: &[u8; 32]) -> [u8; 36] {
    let mut k = [0u8; 36];
    k[..4].copy_from_slice(b"bal/");
    k[4..].copy_from_slice(account);
    k
}

fn allowance_key(owner: &[u8; 32], spender: &[u8; 32]) -> [u8; 71] {
    let mut k = [0u8; 71];
    k[..6].copy_from_slice(b"allow/");
    k[6..38].copy_from_slice(owner);
    k[38] = b'/';
    k[39..71].copy_from_slice(spender);
    k
}

fn op_key(i: u64) -> [u8; 11] {
    let mut k = [0u8; 11];
    k[..3].copy_from_slice(b"op/");
    k[3..].copy_from_slice(&i.to_le_bytes());
    k
}

fn op_stake_key(operator: &[u8; 32]) -> [u8; 41] {
    let mut k = [0u8; 41];
    k[..9].copy_from_slice(b"op_stake/");
    k[9..].copy_from_slice(operator);
    k
}

fn pending_undelegate_key(operator: &[u8; 32]) -> [u8; 36] {
    let mut k = [0u8; 36];
    k[..4].copy_from_slice(b"pue/");
    k[4..].copy_from_slice(operator);
    k
}

fn inflight_undelegations_key(operator: &[u8; 32]) -> [u8; 36] {
    let mut k = [0u8; 36];
    k[..4].copy_from_slice(b"ifu/");
    k[4..].copy_from_slice(operator);
    k
}

fn wq_key(seq: u64) -> [u8; 11] {
    let mut k = [0u8; 11];
    k[..3].copy_from_slice(b"wq/");
    k[3..].copy_from_slice(&seq.to_le_bytes());
    k
}

/// Per-undelegation log entry. Layout: `epoch[8] || amount[16] || operator[32]`
/// = 56 bytes. Used to track which undelegations have matured so that
/// `STAKING_ADDRESS:withdraw` calls can be paired with a precise local
/// `withdrawal_buffer` increment.
fn un_log_key(seq: u64) -> [u8; 11] {
    let mut k = [0u8; 11];
    k[..3].copy_from_slice(b"un/");
    k[3..].copy_from_slice(&seq.to_le_bytes());
    k
}

// ── Storage helpers ──────────────────────────────────────────────

fn get_balance(account: &[u8; 32]) -> u128 {
    storage::get_u128(&balance_key(account)).unwrap_or(0)
}
fn set_balance(account: &[u8; 32], amount: u128) {
    storage::set_u128(&balance_key(account), amount);
}

fn get_allowance(owner: &[u8; 32], spender: &[u8; 32]) -> u128 {
    storage::get_u128(&allowance_key(owner, spender)).unwrap_or(0)
}
fn set_allowance(owner: &[u8; 32], spender: &[u8; 32], amount: u128) {
    storage::set_u128(&allowance_key(owner, spender), amount);
}

fn get_total_supply() -> u128 { storage::get_u128(b"total_supply").unwrap_or(0) }
fn set_total_supply(s: u128) { storage::set_u128(b"total_supply", s); }

fn get_total_pooled() -> u128 { storage::get_u128(b"total_pooled_solen").unwrap_or(0) }
fn set_total_pooled(s: u128) { storage::set_u128(b"total_pooled_solen", s); }

fn get_pending_withdrawals_solen() -> u128 {
    storage::get_u128(b"pending_withdrawal_solen").unwrap_or(0)
}
fn set_pending_withdrawals_solen(s: u128) {
    storage::set_u128(b"pending_withdrawal_solen", s);
}

fn get_op_count() -> u64 { storage::get_u64(b"op_count").unwrap_or(0) }
fn set_op_count(n: u64) { storage::set_u64(b"op_count", n); }

fn get_op_cursor() -> u64 { storage::get_u64(b"op_cursor").unwrap_or(0) }
fn set_op_cursor(c: u64) { storage::set_u64(b"op_cursor", c); }

fn get_op_cap_bps() -> u64 { storage::get_u64(b"op_cap_bps").unwrap_or(DEFAULT_OP_CAP_BPS) }
fn set_op_cap_bps(b: u64) { storage::set_u64(b"op_cap_bps", b); }

fn get_protocol_fee_bps() -> u64 {
    storage::get_u64(b"protocol_fee_bps").unwrap_or(DEFAULT_FEE_BPS)
}
fn set_protocol_fee_bps(b: u64) { storage::set_u64(b"protocol_fee_bps", b); }

fn get_op_stake(op: &[u8; 32]) -> u128 {
    storage::get_u128(&op_stake_key(op)).unwrap_or(0)
}
fn set_op_stake(op: &[u8; 32], amount: u128) {
    storage::set_u128(&op_stake_key(op), amount);
}

fn get_pending_undelegate(op: &[u8; 32]) -> u128 {
    storage::get_u128(&pending_undelegate_key(op)).unwrap_or(0)
}
fn set_pending_undelegate(op: &[u8; 32], amount: u128) {
    storage::set_u128(&pending_undelegate_key(op), amount);
}

fn get_inflight_undelegations(op: &[u8; 32]) -> u64 {
    storage::get_u64(&inflight_undelegations_key(op)).unwrap_or(0)
}
fn set_inflight_undelegations(op: &[u8; 32], n: u64) {
    storage::set_u64(&inflight_undelegations_key(op), n);
}

fn read_32(key: &[u8]) -> Option<[u8; 32]> {
    let data = storage::get(key)?;
    if data.len() < 32 { return None; }
    let mut out = [0u8; 32];
    out.copy_from_slice(&data[..32]);
    Some(out)
}

fn get_owner() -> [u8; 32] { read_32(b"owner").unwrap_or([0u8; 32]) }
fn set_owner(o: &[u8; 32]) { storage::set(b"owner", o); }

fn get_treasury() -> [u8; 32] { read_32(b"treasury").unwrap_or([0u8; 32]) }
fn set_treasury(t: &[u8; 32]) { storage::set(b"treasury", t); }

fn get_slash_oracle() -> [u8; 32] { read_32(b"slash_oracle").unwrap_or([0u8; 32]) }
fn set_slash_oracle(o: &[u8; 32]) { storage::set(b"slash_oracle", o); }

fn is_paused() -> bool {
    matches!(storage::get(b"paused").and_then(|d| d.first().copied()), Some(1))
}
fn set_paused(p: bool) {
    storage::set(b"paused", &[if p { 1 } else { 0 }]);
}

fn current_epoch() -> u64 {
    sdk::block_height() / EPOCH_LENGTH_BLOCKS
}

// ── Withdrawal queue + undelegation log ──────────────────────────

fn get_wq_head() -> u64 { storage::get_u64(b"wq_head").unwrap_or(0) }
fn set_wq_head(s: u64) { storage::set_u64(b"wq_head", s); }
fn get_wq_tail() -> u64 { storage::get_u64(b"wq_tail").unwrap_or(0) }
fn set_wq_tail(s: u64) { storage::set_u64(b"wq_tail", s); }

fn get_un_log_head() -> u64 { storage::get_u64(b"un_log_head").unwrap_or(0) }
fn set_un_log_head(s: u64) { storage::set_u64(b"un_log_head", s); }
fn get_un_log_tail() -> u64 { storage::get_u64(b"un_log_tail").unwrap_or(0) }
fn set_un_log_tail(s: u64) { storage::set_u64(b"un_log_tail", s); }

fn get_withdrawal_buffer() -> u128 {
    storage::get_u128(b"withdrawal_buffer").unwrap_or(0)
}
fn set_withdrawal_buffer(b: u128) { storage::set_u128(b"withdrawal_buffer", b); }

/// Read a withdrawal queue entry. Returns `(account, solen_owed, requested_epoch)`.
fn read_wq(seq: u64) -> Option<([u8; 32], u128, u64)> {
    let data = storage::get(&wq_key(seq))?;
    if data.len() < 56 { return None; }
    let mut account = [0u8; 32];
    account.copy_from_slice(&data[..32]);
    let mut owed = [0u8; 16];
    owed.copy_from_slice(&data[32..48]);
    let mut epoch = [0u8; 8];
    epoch.copy_from_slice(&data[48..56]);
    Some((account, u128::from_le_bytes(owed), u64::from_le_bytes(epoch)))
}

fn write_wq(seq: u64, account: &[u8; 32], solen_owed: u128, requested_epoch: u64) {
    let mut data = [0u8; 56];
    data[..32].copy_from_slice(account);
    data[32..48].copy_from_slice(&solen_owed.to_le_bytes());
    data[48..56].copy_from_slice(&requested_epoch.to_le_bytes());
    storage::set(&wq_key(seq), &data);
}

/// Tombstone a wq entry. Storage doesn't expose delete; setting an empty
/// value gets the entry's `len` to 0 so `read_wq` rejects it.
fn clear_wq(seq: u64) {
    storage::set(&wq_key(seq), &[]);
}

fn read_un_log(seq: u64) -> Option<(u64, u128, [u8; 32])> {
    let data = storage::get(&un_log_key(seq))?;
    if data.len() < 56 { return None; }
    let mut epoch = [0u8; 8];
    epoch.copy_from_slice(&data[..8]);
    let mut amount = [0u8; 16];
    amount.copy_from_slice(&data[8..24]);
    let mut operator = [0u8; 32];
    operator.copy_from_slice(&data[24..56]);
    Some((u64::from_le_bytes(epoch), u128::from_le_bytes(amount), operator))
}

fn write_un_log(seq: u64, epoch: u64, amount: u128, operator: &[u8; 32]) {
    let mut data = [0u8; 56];
    data[..8].copy_from_slice(&epoch.to_le_bytes());
    data[8..24].copy_from_slice(&amount.to_le_bytes());
    data[24..56].copy_from_slice(operator);
    storage::set(&un_log_key(seq), &data);
}

fn clear_un_log(seq: u64) {
    storage::set(&un_log_key(seq), &[]);
}

/// Walk un_log forward from head, summing matured entries (epoch + UNBONDING
/// <= current_epoch). Optionally commits: when `commit` is true, advances
/// `un_log_head`, tombstones consumed entries, and decrements the matching
/// `inflight_undelegations[op]` counters.
///
/// Returns the matured total — same value either way, so callers can do a
/// peek-then-commit pattern: peek to see if buffer + matured covers a
/// pending claim, then commit only if proceeding.
fn walk_matured_log(now: u64, commit: bool) -> u128 {
    let head = get_un_log_head();
    let tail = get_un_log_tail();
    let mut matured = 0u128;
    let mut new_head = head;
    let mut seq = head;
    while seq < tail {
        let entry = match read_un_log(seq) {
            Some(e) => e,
            None => {
                // Tombstoned (shouldn't happen between head and tail in
                // practice, but be safe). Skip past it.
                if commit {
                    new_head = seq + 1;
                }
                seq += 1;
                continue;
            }
        };
        let (epoch, amount, operator) = entry;
        if epoch + UNBONDING_EPOCHS > now {
            // First non-matured — entries are appended in order, so all
            // subsequent are also non-matured. Stop.
            break;
        }
        matured = matured.saturating_add(amount);
        if commit {
            clear_un_log(seq);
            let inflight = get_inflight_undelegations(&operator);
            set_inflight_undelegations(&operator, inflight.saturating_sub(1));
            new_head = seq + 1;
        }
        seq += 1;
    }
    if commit {
        set_un_log_head(new_head);
    }
    matured
}

// ── Arg parsing ──────────────────────────────────────────────────

fn read_account(args: &[u8], offset: usize) -> Option<[u8; 32]> {
    if args.len() < offset + 32 { return None; }
    let mut a = [0u8; 32];
    a.copy_from_slice(&args[offset..offset + 32]);
    Some(a)
}

fn read_u128(args: &[u8], offset: usize) -> Option<u128> {
    if args.len() < offset + 16 { return None; }
    let mut b = [0u8; 16];
    b.copy_from_slice(&args[offset..offset + 16]);
    Some(u128::from_le_bytes(b))
}

fn read_u64(args: &[u8], offset: usize) -> Option<u64> {
    if args.len() < offset + 8 { return None; }
    let mut b = [0u8; 8];
    b.copy_from_slice(&args[offset..offset + 8]);
    Some(u64::from_le_bytes(b))
}

// ── Entry point ──────────────────────────────────────────────────

#[no_mangle]
pub extern "C" fn call(input_ptr: i32, input_len: i32) -> i32 {
    let input = sdk::read_input(input_ptr, input_len);
    let null_pos = input.iter().position(|&b| b == 0).unwrap_or(input.len());
    let method = &input[..null_pos];
    let args = if null_pos + 1 < input.len() {
        &input[null_pos + 1..]
    } else {
        &[]
    };

    match method {
        // SRC-20
        b"init" => do_init(args),
        b"transfer" => do_transfer(args),
        b"transfer_from" => do_transfer_from(args),
        b"approve" => do_approve(args),
        b"balance_of" => do_balance_of(args),
        b"allowance" => do_allowance(args),
        b"total_supply" => do_total_supply(),
        b"name" => sdk::return_value(NAME),
        b"symbol" => sdk::return_value(SYMBOL),
        b"decimals" => sdk::return_value(&[DECIMALS]),

        // Staking lifecycle
        b"deposit" => do_deposit(),
        b"request_withdrawal" => do_request_withdrawal(args),
        b"claim_withdrawal" => do_claim_withdrawal(args),
        b"crank_undelegations" => do_crank_undelegations(),
        b"recompound_rewards" => do_recompound_rewards(),
        b"poke" => do_poke(),

        // Slashing oracle
        b"report_slash" => do_report_slash(args),

        // Admin
        b"set_operator" => do_set_operator(args),
        b"remove_operator" => do_remove_operator(args),
        b"set_op_count" => do_admin_set_op_count(args),
        b"set_op_cap_bps" => do_set_op_cap_bps(args),
        b"set_protocol_fee_bps" => do_set_protocol_fee_bps(args),
        b"set_treasury" => do_set_treasury(args),
        b"set_slash_oracle" => do_set_slash_oracle(args),
        b"pause" => do_pause(),
        b"unpause" => do_unpause(),
        b"settle_shortfall" => do_settle_shortfall(args),

        // Reads
        b"exchange_rate" => do_exchange_rate(),
        b"pending_undelegate_op_of" => do_pending_undelegate_op_of(args),
        b"op_stake_of" => do_op_stake_of(args),
        b"withdrawal_at" => do_withdrawal_at(args),
        b"pending_withdrawals_of" => do_pending_withdrawals_of(args),
        b"owner" => sdk::return_value(&get_owner()),
        b"treasury" => sdk::return_value(&get_treasury()),
        b"slash_oracle" => sdk::return_value(&get_slash_oracle()),
        b"paused" => sdk::return_value(&[if is_paused() { 1 } else { 0 }]),

        _ => sdk::return_value(b"err:unknown_method"),
    }
}

// ── SRC-20 ───────────────────────────────────────────────────────

/// `init(treasury[32] || slash_oracle[32])` — caller becomes owner. Defaults
/// applied to fee/cap. Operator set must be populated separately via
/// `set_op_count` + `set_operator(i, op)`.
fn do_init(args: &[u8]) -> i32 {
    if get_owner() != [0u8; 32] {
        return sdk::return_value(b"err:already_initialized");
    }
    let treasury = match read_account(args, 0) {
        Some(a) => a,
        None => return sdk::return_value(b"err:invalid_args"),
    };
    let oracle = match read_account(args, 32) {
        Some(a) => a,
        None => return sdk::return_value(b"err:invalid_args"),
    };
    let caller = sdk::caller();
    set_owner(&caller);
    set_treasury(&treasury);
    set_slash_oracle(&oracle);
    set_protocol_fee_bps(DEFAULT_FEE_BPS);
    set_op_cap_bps(DEFAULT_OP_CAP_BPS);
    events::emit(b"initialized", &caller);
    sdk::return_value(b"ok")
}

fn do_transfer(args: &[u8]) -> i32 {
    let caller = sdk::caller();
    let to = match read_account(args, 0) { Some(a) => a, None => return sdk::return_value(b"err:invalid_args") };
    let amount = match read_u128(args, 32) { Some(a) => a, None => return sdk::return_value(b"err:invalid_args") };

    let from_bal = get_balance(&caller);
    if from_bal < amount {
        return sdk::return_value(b"err:insufficient_balance");
    }
    set_balance(&caller, from_bal - amount);
    set_balance(&to, get_balance(&to) + amount);

    let mut data = [0u8; 48];
    data[..32].copy_from_slice(&to);
    data[32..].copy_from_slice(&amount.to_le_bytes());
    events::emit(b"transfer", &data);
    sdk::return_value(b"ok")
}

fn do_transfer_from(args: &[u8]) -> i32 {
    let caller = sdk::caller();
    let from = match read_account(args, 0) { Some(a) => a, None => return sdk::return_value(b"err:invalid_args") };
    let to = match read_account(args, 32) { Some(a) => a, None => return sdk::return_value(b"err:invalid_args") };
    let amount = match read_u128(args, 64) { Some(a) => a, None => return sdk::return_value(b"err:invalid_args") };

    let allowed = get_allowance(&from, &caller);
    if allowed < amount { return sdk::return_value(b"err:insufficient_allowance"); }
    let from_bal = get_balance(&from);
    if from_bal < amount { return sdk::return_value(b"err:insufficient_balance"); }

    set_balance(&from, from_bal - amount);
    set_balance(&to, get_balance(&to) + amount);
    set_allowance(&from, &caller, allowed - amount);

    let mut data = [0u8; 48];
    data[..32].copy_from_slice(&to);
    data[32..].copy_from_slice(&amount.to_le_bytes());
    events::emit(b"transfer", &data);
    sdk::return_value(b"ok")
}

fn do_approve(args: &[u8]) -> i32 {
    let caller = sdk::caller();
    let spender = match read_account(args, 0) { Some(a) => a, None => return sdk::return_value(b"err:invalid_args") };
    let amount = match read_u128(args, 32) { Some(a) => a, None => return sdk::return_value(b"err:invalid_args") };
    set_allowance(&caller, &spender, amount);

    // F-08 audit fix: emit owner ‖ spender ‖ amount so indexers can
    // reconstruct allowances from the event log alone.
    let mut data = [0u8; 80];
    data[..32].copy_from_slice(&caller);
    data[32..64].copy_from_slice(&spender);
    data[64..80].copy_from_slice(&amount.to_le_bytes());
    events::emit(b"approval", &data);
    sdk::return_value(b"ok")
}

fn do_balance_of(args: &[u8]) -> i32 {
    let account = match read_account(args, 0) { Some(a) => a, None => return sdk::return_value(b"err:invalid_args") };
    sdk::return_value(&get_balance(&account).to_le_bytes())
}

fn do_allowance(args: &[u8]) -> i32 {
    let owner = match read_account(args, 0) { Some(a) => a, None => return sdk::return_value(b"err:invalid_args") };
    let spender = match read_account(args, 32) { Some(a) => a, None => return sdk::return_value(b"err:invalid_args") };
    sdk::return_value(&get_allowance(&owner, &spender).to_le_bytes())
}

fn do_total_supply() -> i32 { sdk::return_value(&get_total_supply().to_le_bytes()) }

// ── Staking lifecycle ────────────────────────────────────────────

/// Detect rewards by comparing the contract's current balance against the
/// snapshot taken at the end of the previous sync. Skim the protocol fee in
/// stSOLEN to the treasury (at the post-growth rate, biasing a tiny ~0.1%
/// loss onto treasury rather than holders — see spec §11 Q5), grow
/// `total_pooled_solen` by the remainder, and refresh the snapshot.
///
/// Snapshots the *current* balance (including msg_value); callers that queue
/// outflows post-return must subtract the queued amount from
/// `last_balance_seen` themselves so the next sync doesn't double-count.
///
/// **Limitation (v1 scaffold):** matured-undelegation inflows from the
/// staking system contract aren't tracked yet — only deposits and rewards are
/// expected to land in the account balance. Withdrawal-buffer tracking comes
/// in the next pass alongside `claim_withdrawal`.
fn sync_rewards() {
    let current = sdk::self_balance();
    let last = storage::get_u128(b"last_balance_seen").unwrap_or(0);
    let msg_value = sdk::msg_value();

    let inflow = current
        .saturating_sub(last)
        .saturating_sub(msg_value);

    if inflow > 0 {
        let fee_bps = get_protocol_fee_bps() as u128;
        let fee_solen = inflow.saturating_mul(fee_bps) / 10_000;
        let growth_solen = inflow - fee_solen;

        let pool_before = get_total_pooled();
        let pool_after = pool_before + growth_solen;
        set_total_pooled(pool_after);

        // Mint fee-stSOLEN to treasury at the post-growth rate. Symmetrical
        // with the deposit math: `mint = solen * supply / pool`.
        let supply = get_total_supply();
        if fee_solen > 0 && pool_after > 0 && supply > 0 {
            let fee_mint = fee_solen.saturating_mul(supply) / pool_after;
            if fee_mint > 0 {
                let treasury = get_treasury();
                set_balance(&treasury, get_balance(&treasury) + fee_mint);
                set_total_supply(supply + fee_mint);

                let mut data = [0u8; 48];
                data[..32].copy_from_slice(&treasury);
                data[32..].copy_from_slice(&fee_mint.to_le_bytes());
                events::emit(b"mint", &data);
            }
        }
    }

    storage::set_u128(b"last_balance_seen", current);
}

/// Round-robin operator selection with cap-skipping (spec §6).
///
/// The cap is computed against `pool_after = total_pooled_solen + amount` so
/// that the very first deposit (when pool == 0) doesn't trivially fit every
/// operator under cap. If every operator would be over cap, returns the
/// cursor's pick anyway — the depositor isn't blocked by tail conditions.
///
/// Returns `[0u8; 32]` if the allowlist is empty or all slots are zeroed.
fn pick_operator(amount: u128) -> [u8; 32] {
    let (chosen, advance_to) = select_operator(amount);
    if chosen != [0u8; 32] {
        set_op_cursor(advance_to);
    }
    chosen
}

/// Read-only variant of `pick_operator` — same selection logic, but does NOT
/// advance `op_cursor`. Used during deposit's validation phase (audit fix
/// F-01) so we can confirm an operator is available before committing any
/// state mutation.
fn peek_operator(amount: u128) -> [u8; 32] {
    select_operator(amount).0
}

/// Internal: returns `(chosen, next_cursor)`. The caller decides whether to
/// commit the cursor advance.
fn select_operator(amount: u128) -> ([u8; 32], u64) {
    let count = get_op_count();
    let cursor = get_op_cursor();
    if count == 0 {
        return ([0u8; 32], cursor);
    }
    let cap_bps = get_op_cap_bps() as u128;
    let pool_after = get_total_pooled().saturating_add(amount);
    let cap = pool_after.saturating_mul(cap_bps) / 10_000;

    let mut walk = cursor;
    let mut fallback = [0u8; 32];
    for _ in 0..count {
        let op = read_32(&op_key(walk)).unwrap_or([0u8; 32]);
        let next_cursor = (walk + 1) % count;
        if op != [0u8; 32] {
            if fallback == [0u8; 32] {
                fallback = op;
            }
            if get_op_stake(&op).saturating_add(amount) <= cap {
                return (op, next_cursor);
            }
        }
        walk = next_cursor;
    }
    // Every populated slot is over cap. Accept the first non-empty one we saw
    // rather than returning a sentinel — depositors shouldn't be blocked by a
    // pool-wide saturation event.
    if fallback != [0u8; 32] {
        return (fallback, (cursor + 1) % count);
    }
    ([0u8; 32], cursor)
}

/// `deposit()` — payable. Reads `msg_value()` for the SOLEN-in.
///
/// Mints stSOLEN at the current exchange rate (or 1:1 minus a 1000-unit
/// dead-address burn on the very first deposit, per spec §5), then queues
/// `STAKING_ADDRESS:delegate(operator || to_delegate)` for the
/// round-robin-selected operator.
///
/// **Reserve top-up:** the staking call requires
/// `caller.balance >= amount + MIN_FEE_RESERVE`. To satisfy that, the
/// contract retains `MIN_FEE_RESERVE` permanently and only delegates the
/// surplus over reserve. On a fresh contract (balance == msg_value), this
/// means the first `MIN_FEE_RESERVE` of msg_value goes toward establishing
/// the reserve and the remainder is delegated. The reserve is principal —
/// it still backs stSOLEN — so `total_pooled_solen` grows by the full
/// `msg_value`, not just `to_delegate`.
fn do_deposit() -> i32 {
    // ── Validation phase: NO storage mutation may happen here. The Solen VM
    // does not roll back state on `err:*` return values for single-action ops;
    // any err returned after a write would persist a partial state.
    if is_paused() {
        return sdk::return_value(b"err:paused");
    }
    let msg_value = sdk::msg_value();
    if msg_value == 0 {
        return sdk::return_value(b"err:zero_value");
    }

    let bal_now = sdk::self_balance();
    let to_delegate = bal_now
        .saturating_sub(MIN_FEE_RESERVE)
        .min(msg_value);
    if to_delegate == 0 {
        return sdk::return_value(b"err:deposit_too_small_for_reserve");
    }

    let supply_pre = get_total_supply();
    let is_first_deposit = supply_pre == 0;
    if is_first_deposit && msg_value < MIN_FIRST_DEPOSIT {
        return sdk::return_value(b"err:first_deposit_too_small");
    }

    // Operator availability — must precede any state mutation. F-01 audit fix.
    if get_op_count() == 0 {
        return sdk::return_value(b"err:no_operators");
    }
    // Peek at operator selection without committing the cursor advance.
    let chosen_preview = peek_operator(to_delegate);
    if chosen_preview == [0u8; 32] {
        return sdk::return_value(b"err:no_operators");
    }

    // ── Mutation phase. After this point all paths must succeed; remaining
    // failure modes (queue full, staking call rejection) are unreachable in
    // practice and use `panic!` to force a VM trap → UserOp rollback.
    sync_rewards();

    // Re-read after sync (sync may have absorbed rewards into total_pooled_solen
    // and minted treasury fee, changing supply).
    let supply = get_total_supply();
    let pool = get_total_pooled();

    let mint_amount = if is_first_deposit {
        // Bootstrap burn: locks 1000 stSOLEN to a dead address (Uniswap V2 trick)
        // to prevent the donate-then-deposit rate-inflation attack.
        let dead_bal = get_balance(&DEAD_ADDRESS);
        set_balance(&DEAD_ADDRESS, dead_bal + BOOTSTRAP_BURN);
        set_total_supply(BOOTSTRAP_BURN);

        let mut data = [0u8; 48];
        data[..32].copy_from_slice(&DEAD_ADDRESS);
        data[32..].copy_from_slice(&BOOTSTRAP_BURN.to_le_bytes());
        events::emit(b"mint", &data);

        msg_value - BOOTSTRAP_BURN
    } else {
        if pool == 0 {
            // Should be unreachable — supply > 0 implies pool > 0 by invariant
            // I2. Treat as panic-class.
            panic!("stSOLEN: invariant violation pool=0 supply>0");
        }
        msg_value.saturating_mul(supply) / pool
    };

    // Sanity — mint_amount could be zero only on a dust deposit against a huge
    // pool. Caught here as the final check before mutation; harmless to abort
    // since we haven't committed the user-facing mutations yet (the bootstrap
    // burn above is the only mutation, and it only runs in the else branch).
    if mint_amount == 0 && !is_first_deposit {
        return sdk::return_value(b"err:mint_zero");
    }

    // Commit the chosen operator (advance op_cursor).
    let chosen = pick_operator(to_delegate);

    let caller = sdk::caller();
    set_balance(&caller, get_balance(&caller) + mint_amount);
    set_total_supply(get_total_supply() + mint_amount);
    set_total_pooled(pool + msg_value);
    set_op_stake(&chosen, get_op_stake(&chosen) + to_delegate);

    let mut delegate_args = [0u8; 48];
    delegate_args[..32].copy_from_slice(&chosen);
    delegate_args[32..].copy_from_slice(&to_delegate.to_le_bytes());
    if !sdk::queue_call(&STAKING_ADDRESS, b"delegate", &delegate_args) {
        // F-05: post-mutation queue exhaustion. Panic forces VM trap → UserOp
        // rollback, which is the only correct behavior since we've already
        // mutated state. Unreachable in practice (queue cap=16, we use 1).
        panic!("stSOLEN: delegate queue full");
    }

    // Adjust last_balance_seen to reflect the post-return balance: the queued
    // delegate will subtract `to_delegate`.
    let last_seen = storage::get_u128(b"last_balance_seen").unwrap_or(0);
    storage::set_u128(b"last_balance_seen", last_seen.saturating_sub(to_delegate));

    let mut data = [0u8; 96];
    data[..32].copy_from_slice(&caller);
    data[32..48].copy_from_slice(&msg_value.to_le_bytes());
    data[48..64].copy_from_slice(&mint_amount.to_le_bytes());
    data[64..96].copy_from_slice(&chosen);
    events::emit(b"deposit", &data);

    let mut mint_data = [0u8; 48];
    mint_data[..32].copy_from_slice(&caller);
    mint_data[32..].copy_from_slice(&mint_amount.to_le_bytes());
    events::emit(b"mint", &mint_data);

    sdk::return_value(b"ok")
}

/// `request_withdrawal(stsolen_amount[16])` — caller burns stSOLEN, locks the
/// owed SOLEN at the current rate, and enqueues a withdrawal claim. Allocates
/// the burn pro-rata across operators into `pending_undelegate_op[]` so a
/// later `crank_undelegations` can settle it via the staking system contract.
///
/// Does *not* call `STAKING_ADDRESS:undelegate` here — batching across users
/// via the cranker keeps us comfortably under the staking module's 7-row
/// per-(delegator,validator) limit.
fn do_request_withdrawal(args: &[u8]) -> i32 {
    // ── Validation phase: NO storage mutation. F-02 audit fix. The Solen VM
    // does not roll back state on `err:*` returns for single-action ops, so
    // any err here must precede any write.
    if is_paused() {
        return sdk::return_value(b"err:paused");
    }
    let stsolen_burn = match read_u128(args, 0) {
        Some(a) => a,
        None => return sdk::return_value(b"err:invalid_args"),
    };
    if stsolen_burn == 0 {
        return sdk::return_value(b"err:zero_amount");
    }
    let caller = sdk::caller();
    let bal = get_balance(&caller);
    if bal < stsolen_burn {
        return sdk::return_value(b"err:insufficient_balance");
    }

    let supply_pre = get_total_supply();
    let pool_pre = get_total_pooled();
    if supply_pre == 0 || pool_pre == 0 {
        return sdk::return_value(b"err:empty_pool");
    }
    // Pre-sync sanity check on owed; we re-derive below post-sync.
    if stsolen_burn.saturating_mul(pool_pre) / supply_pre == 0 {
        return sdk::return_value(b"err:owed_zero");
    }

    // Verify there is delegated stake to undelegate against, BEFORE burning
    // the user's stSOLEN. This is the F-02 fix.
    let count = get_op_count();
    let mut total_op_stake: u128 = 0;
    for i in 0..count {
        let op = read_32(&op_key(i)).unwrap_or([0u8; 32]);
        if op == [0u8; 32] {
            continue;
        }
        total_op_stake = total_op_stake.saturating_add(get_op_stake(&op));
    }
    if total_op_stake == 0 {
        return sdk::return_value(b"err:no_delegated_stake");
    }

    // ── Mutation phase. All preconditions are validated.
    sync_rewards();

    // Re-read after sync; the rate may have moved if rewards landed.
    let supply = get_total_supply();
    let pool = get_total_pooled();
    let solen_owed = stsolen_burn.saturating_mul(pool) / supply;
    if solen_owed == 0 {
        // Rate moved enough that owed truncates to 0. Treat as benign:
        // sync_rewards has already absorbed any pending rewards (no harm),
        // so we just abort without burning.
        return sdk::return_value(b"err:owed_zero");
    }
    // Re-read total_op_stake post-sync — sync doesn't touch op_stake but be
    // defensive (a slash report between pre-check and here would only reduce
    // it, so the pre-check still bounds the post-sync invariant).

    // Burn stSOLEN.
    set_balance(&caller, get_balance(&caller) - stsolen_burn);
    set_total_supply(supply - stsolen_burn);

    // Pool shrinks; pending bookkeeping grows.
    set_total_pooled(pool - solen_owed);
    set_pending_withdrawals_solen(get_pending_withdrawals_solen() + solen_owed);

    // Allocate pro-rata across operators. Residual to last non-empty op.
    let mut allocated: u128 = 0;
    let mut last_op = [0u8; 32];
    for i in 0..count {
        let op = read_32(&op_key(i)).unwrap_or([0u8; 32]);
        if op == [0u8; 32] {
            continue;
        }
        let stake = get_op_stake(&op);
        if stake == 0 {
            continue;
        }
        let share = solen_owed.saturating_mul(stake) / total_op_stake;
        if share > 0 {
            set_pending_undelegate(&op, get_pending_undelegate(&op) + share);
            allocated = allocated.saturating_add(share);
        }
        last_op = op;
    }
    if allocated < solen_owed && last_op != [0u8; 32] {
        let residual = solen_owed - allocated;
        set_pending_undelegate(&last_op, get_pending_undelegate(&last_op) + residual);
    }

    // Append to the withdrawal queue.
    let seq = get_wq_tail();
    let now = current_epoch();
    write_wq(seq, &caller, solen_owed, now);
    set_wq_tail(seq + 1);

    let eligible = now + UNBONDING_EPOCHS + 1;
    let mut data = [0u8; 64];
    data[..32].copy_from_slice(&caller);
    data[32..48].copy_from_slice(&stsolen_burn.to_le_bytes());
    data[48..56].copy_from_slice(&seq.to_le_bytes());
    data[56..64].copy_from_slice(&eligible.to_le_bytes());
    events::emit(b"withdrawal_requested", &data);

    sdk::return_value(b"ok")
}

/// `crank_undelegations()` — permissionless. Pulls any matured undelegations
/// from the staking system contract (queueing `STAKING_ADDRESS:withdraw` once
/// for the whole batch), then drains `pending_undelegate_op[]` per operator
/// into fresh `STAKING_ADDRESS:undelegate(op, amount)` calls — each one
/// logged so a later claim knows when its share matures.
///
/// Skips operators already at `MAX_UNDELEGATIONS_PER_OP - 1` in-flight (one
/// slot of headroom prevents the cranker from being self-blocked under
/// concurrent activity).
fn do_crank_undelegations() -> i32 {
    sync_rewards();

    let now = current_epoch();

    // Drain any matured first — this both frees inflight slots and tops up
    // the buffer before we queue more undelegations.
    let matured = walk_matured_log(now, /* commit = */ true);
    if matured > 0 {
        if !sdk::queue_call(&STAKING_ADDRESS, b"withdraw", &[]) {
            // F-05: post-mutation queue exhaustion. Panic forces VM trap →
            // UserOp rollback so the drained log entries / freed inflight
            // slots aren't permanently committed without the matching
            // withdraw call. Unreachable in practice (queue cap=16, used 1).
            panic!("stSOLEN: withdraw queue full");
        }
        set_withdrawal_buffer(get_withdrawal_buffer() + matured);
        // Pre-emptively reflect the post-return inflow so sync_rewards in the
        // next op doesn't classify it as a phantom reward.
        let last_seen = storage::get_u128(b"last_balance_seen").unwrap_or(0);
        storage::set_u128(b"last_balance_seen", last_seen + matured);
    }

    // Queue a fresh undelegate per operator with pending amount.
    let count = get_op_count();
    let mut operators_processed: u64 = 0;
    let mut total_undelegated: u128 = 0;
    for i in 0..count {
        let op = read_32(&op_key(i)).unwrap_or([0u8; 32]);
        if op == [0u8; 32] {
            continue;
        }
        let pending = get_pending_undelegate(&op);
        if pending == 0 {
            continue;
        }
        // Headroom-of-1 against the staking module's 7-row cap. Without
        // headroom, a race between cranker invocations could push us right
        // up to the limit and self-block the next crank.
        let inflight = get_inflight_undelegations(&op);
        if inflight + 1 >= MAX_UNDELEGATIONS_PER_OP {
            continue;
        }

        let stake = get_op_stake(&op);
        let amount = pending.min(stake);
        if amount == 0 {
            // Operator stake decreased (e.g. via slash) below the queued
            // amount; carry the difference forward to be redistributed.
            continue;
        }

        set_pending_undelegate(&op, pending - amount);
        set_op_stake(&op, stake - amount);
        set_inflight_undelegations(&op, inflight + 1);

        // Log the undelegation so claim_withdrawal can match maturity.
        let log_seq = get_un_log_tail();
        write_un_log(log_seq, now, amount, &op);
        set_un_log_tail(log_seq + 1);

        let mut undel_args = [0u8; 48];
        undel_args[..32].copy_from_slice(&op);
        undel_args[32..].copy_from_slice(&amount.to_le_bytes());
        if !sdk::queue_call(&STAKING_ADDRESS, b"undelegate", &undel_args) {
            // F-05: per-op state has already been mutated. Panic for
            // VM-trap rollback. Unreachable in practice (cap=16, max=12).
            panic!("stSOLEN: undelegate queue full");
        }
        operators_processed += 1;
        total_undelegated = total_undelegated.saturating_add(amount);
    }

    let mut data = [0u8; 24];
    data[..8].copy_from_slice(&operators_processed.to_le_bytes());
    data[8..24].copy_from_slice(&total_undelegated.to_le_bytes());
    events::emit(b"crank", &data);

    sdk::return_value(b"ok")
}

/// `claim_withdrawal(seq[8])` — FIFO-only. Validates the queue head matches
/// `seq` and eligibility (`current_epoch >= requested_epoch + UNBONDING_EPOCHS + 1`),
/// then pays the claimant from `withdrawal_buffer` via `sdk::transfer`.
///
/// **Buffer must be pre-funded by `crank_undelegations`**. The cranker
/// queues `STAKING_ADDRESS:withdraw` to pull matured undelegations into the
/// contract account and credits `withdrawal_buffer`. Splitting it from
/// `claim_withdrawal` avoids a sharp ordering issue: the executor fires
/// `native_transfers` *before* draining `pending_calls` (executor.rs:1108
/// vs 1162), so a single op that mixed `sdk::transfer` and a queued
/// `withdraw` would attempt the payout before the matured pull.
///
/// Permissionless — anyone can settle the head of the queue. Returns
/// `err:buffer_insufficient` if the cranker hasn't yet topped up the
/// buffer; user retries after a crank.
fn do_claim_withdrawal(args: &[u8]) -> i32 {
    let seq = match read_u64(args, 0) {
        Some(s) => s,
        None => return sdk::return_value(b"err:invalid_args"),
    };
    let head = get_wq_head();
    if seq != head {
        return sdk::return_value(b"err:not_head_of_queue");
    }
    let (account, solen_owed, requested_epoch) = match read_wq(seq) {
        Some(e) => e,
        None => return sdk::return_value(b"err:no_such_request"),
    };
    let now = current_epoch();
    if now < requested_epoch + UNBONDING_EPOCHS + 1 {
        return sdk::return_value(b"err:not_yet_eligible");
    }

    let buffer = get_withdrawal_buffer();
    if buffer < solen_owed {
        return sdk::return_value(b"err:buffer_insufficient");
    }

    // ── Mutation phase. All checks pass; full-pay path is straightforward.
    sync_rewards();

    if !sdk::transfer(&account, solen_owed) {
        // Post-mutation only if sync_rewards mutated; the actual claim writes
        // come below. Panic for VM-trap rollback; unreachable in practice.
        panic!("stSOLEN: claim transfer queue full");
    }
    set_withdrawal_buffer(buffer - solen_owed);
    set_pending_withdrawals_solen(
        get_pending_withdrawals_solen().saturating_sub(solen_owed),
    );

    // Pre-emptively reflect the post-return outflow so sync_rewards in the
    // next op doesn't see a phantom drop.
    let last_seen = storage::get_u128(b"last_balance_seen").unwrap_or(0);
    storage::set_u128(b"last_balance_seen", last_seen.saturating_sub(solen_owed));

    clear_wq(seq);
    set_wq_head(head + 1);

    let mut data = [0u8; 56];
    data[..32].copy_from_slice(&account);
    data[32..48].copy_from_slice(&solen_owed.to_le_bytes());
    data[48..56].copy_from_slice(&seq.to_le_bytes());
    events::emit(b"withdrawal_claimed", &data);

    sdk::return_value(b"ok")
}

/// `settle_shortfall(seq[8])` — owner-only. Force-settle a queue head whose
/// `solen_owed` exceeds `withdrawal_buffer` by paying out *what's available*
/// and writing off the rest. Advances the queue so subsequent claims unblock.
///
/// **F-04 audit fix.** Without this method, a slash-induced shortfall at the
/// queue head permanently blocks every later withdrawal under strict FIFO.
/// Owner-gated rather than permissionless because the user is taking a real
/// haircut — the residual is dropped from the protocol's obligation pool
/// rather than honored later. Owner is responsible for off-chain compensation
/// to the affected user if desired.
///
/// Emits `claim_shortfall(account, seq, owed, paid, shortfall)` (88 bytes).
fn do_settle_shortfall(args: &[u8]) -> i32 {
    if let Err(r) = require_owner() { return r; }
    let seq = match read_u64(args, 0) {
        Some(s) => s,
        None => return sdk::return_value(b"err:invalid_args"),
    };
    let head = get_wq_head();
    if seq != head {
        return sdk::return_value(b"err:not_head_of_queue");
    }
    let (account, solen_owed, requested_epoch) = match read_wq(seq) {
        Some(e) => e,
        None => return sdk::return_value(b"err:no_such_request"),
    };
    let now = current_epoch();
    if now < requested_epoch + UNBONDING_EPOCHS + 1 {
        return sdk::return_value(b"err:not_yet_eligible");
    }

    let buffer = get_withdrawal_buffer();
    let paid = buffer.min(solen_owed);
    let shortfall = solen_owed - paid;

    // ── Mutation phase.
    sync_rewards();

    if paid > 0 {
        if !sdk::transfer(&account, paid) {
            panic!("stSOLEN: shortfall transfer queue full");
        }
        set_withdrawal_buffer(buffer - paid);
        let last_seen = storage::get_u128(b"last_balance_seen").unwrap_or(0);
        storage::set_u128(b"last_balance_seen", last_seen.saturating_sub(paid));
    }

    // Resolve the queue obligation in full from the protocol's perspective.
    // The shortfall is dropped — the protocol's books reflect that this seq
    // is settled. The user lost real value; ops should handle compensation
    // off-chain if appropriate.
    set_pending_withdrawals_solen(
        get_pending_withdrawals_solen().saturating_sub(solen_owed),
    );

    clear_wq(seq);
    set_wq_head(head + 1);

    let mut data = [0u8; 88];
    data[..32].copy_from_slice(&account);
    data[32..40].copy_from_slice(&seq.to_le_bytes());
    data[40..56].copy_from_slice(&solen_owed.to_le_bytes());
    data[56..72].copy_from_slice(&paid.to_le_bytes());
    data[72..88].copy_from_slice(&shortfall.to_le_bytes());
    events::emit(b"claim_shortfall", &data);

    sdk::return_value(b"ok")
}

/// `recompound_rewards()` — re-delegate idle reward SOLEN sitting in the
/// contract account. Permissionless; rate-limited to once per epoch so the
/// staking contract isn't spammed with tiny delegations.
///
/// Conservatively skips when `available < 100 SOLEN` (10^10 base units): not
/// worth the gas for sub-100-SOLEN dust until rewards accumulate.
fn do_recompound_rewards() -> i32 {
    // ── Validation phase: NO storage mutation. F-05 + F-07 audit fix.
    let now = current_epoch();
    let last_recomp = storage::get_u64(b"last_recompound_epoch").unwrap_or(0);
    // Rate-limit: skip if a recompound has already happened this epoch. The
    // `last_recomp != 0` guard distinguishes "first ever" from "already done
    // this epoch"; without it, the first recompound at epoch 0 would be
    // blocked. F-07 audit fix.
    if last_recomp != 0 && now <= last_recomp {
        return sdk::return_value(b"err:rate_limited");
    }

    if get_op_count() == 0 {
        return sdk::return_value(b"err:no_operators");
    }
    if peek_operator(0) == [0u8; 32] {
        return sdk::return_value(b"err:no_operators");
    }

    // ── Mutation phase. Validation has confirmed an operator exists.
    sync_rewards();

    let bal = sdk::self_balance();
    let pending = get_pending_withdrawals_solen();
    let available = bal
        .saturating_sub(pending)
        .saturating_sub(MIN_FEE_RESERVE);

    const MIN_RECOMPOUND: u128 = 100 * 100_000_000; // 100 SOLEN
    if available < MIN_RECOMPOUND {
        // sync_rewards may have moved state but no recompound-specific
        // mutation has happened; safe to err.
        return sdk::return_value(b"err:insufficient_to_recompound");
    }

    let chosen = pick_operator(available);

    set_op_stake(&chosen, get_op_stake(&chosen) + available);
    // total_pooled_solen already reflects this SOLEN — sync_rewards absorbed
    // it as growth on the way in. Re-delegating doesn't change the pool size,
    // just where it lives (account balance → staking contract).

    let mut args = [0u8; 48];
    args[..32].copy_from_slice(&chosen);
    args[32..].copy_from_slice(&available.to_le_bytes());
    if !sdk::queue_call(&STAKING_ADDRESS, b"delegate", &args) {
        // F-05: post-mutation queue exhaustion. Panic for VM-trap rollback.
        panic!("stSOLEN: recompound delegate queue full");
    }

    // The delegate will deduct `available` from balance post-return; refresh
    // the snapshot so the next sync doesn't double-count it as a "missing"
    // outflow.
    storage::set_u128(b"last_balance_seen", bal.saturating_sub(available));
    storage::set_u64(b"last_recompound_epoch", now);

    let mut data = [0u8; 48];
    data[..16].copy_from_slice(&available.to_le_bytes());
    data[16..].copy_from_slice(&chosen);
    events::emit(b"recompounded", &data);

    sdk::return_value(b"ok")
}

/// `poke()` — no-op call that just runs `sync_rewards`. Lets anyone refresh
/// `total_pooled_solen` so `exchange_rate()` reads stay current during quiet
/// periods.
fn do_poke() -> i32 {
    sync_rewards();
    sdk::return_value(b"ok")
}

// ── Slashing oracle ──────────────────────────────────────────────

/// `report_slash(operator[32] || realized_stake[16])` — oracle-key-gated.
/// Reduces `op_stake[operator]` and `total_pooled_solen` by the loss when
/// off-chain monitoring detects a `slashed` event for an allowlisted operator.
fn do_report_slash(args: &[u8]) -> i32 {
    let caller = sdk::caller();
    let oracle = get_slash_oracle();
    if oracle == [0u8; 32] || caller != oracle {
        return sdk::return_value(b"err:unauthorized");
    }
    let operator = match read_account(args, 0) { Some(a) => a, None => return sdk::return_value(b"err:invalid_args") };
    let realized = match read_u128(args, 32) { Some(a) => a, None => return sdk::return_value(b"err:invalid_args") };

    let prior = get_op_stake(&operator);
    if realized >= prior {
        // Oracle reported stale or non-loss data — reject so we don't accidentally
        // inflate the pool.
        return sdk::return_value(b"err:not_a_loss");
    }
    let loss = prior - realized;
    set_op_stake(&operator, realized);
    let pool = get_total_pooled();
    set_total_pooled(pool.saturating_sub(loss));

    let mut data = [0u8; 80];
    data[..32].copy_from_slice(&operator);
    data[32..48].copy_from_slice(&prior.to_le_bytes());
    data[48..64].copy_from_slice(&realized.to_le_bytes());
    data[64..80].copy_from_slice(&loss.to_le_bytes());
    events::emit(b"slash_reported", &data);
    sdk::return_value(b"ok")
}

// ── Admin (owner-gated) ──────────────────────────────────────────

fn require_owner() -> Result<(), i32> {
    if sdk::caller() != get_owner() {
        return Err(sdk::return_value(b"err:unauthorized"));
    }
    Ok(())
}

/// `set_operator(index[8] || operator[32])` — write or replace allowlist slot.
/// Initializes `op_stake[operator] = 0` if it was unset.
/// `set_operator(index[8] || operator[32])` — install an operator at a slot.
///
/// **Refuses to overwrite a slot whose current occupant has non-zero state**
/// (`op_stake > 0`, pending undelegate > 0, or in-flight undelegations > 0).
/// F-03 audit fix: the previous implementation would orphan the old
/// operator's accounting outside the active allowlist, breaking
/// withdrawal-allocation invariants. To replace a busy operator, the owner
/// must first drain the old position via the normal withdrawal path or use
/// the migration helper (TODO v1.1).
fn do_set_operator(args: &[u8]) -> i32 {
    if let Err(r) = require_owner() { return r; }
    let i = match read_u64(args, 0) { Some(v) => v, None => return sdk::return_value(b"err:invalid_args") };
    let op = match read_account(args, 8) { Some(a) => a, None => return sdk::return_value(b"err:invalid_args") };
    if i >= MAX_OPERATORS { return sdk::return_value(b"err:index_out_of_range"); }

    // Reject if the existing slot occupant still has accounting we'd strand.
    let existing = read_32(&op_key(i)).unwrap_or([0u8; 32]);
    if existing != [0u8; 32] && existing != op {
        if has_active_position(&existing) {
            return sdk::return_value(b"err:slot_occupant_has_position");
        }
    }

    storage::set(&op_key(i), &op);

    let mut data = [0u8; 40];
    data[..8].copy_from_slice(&i.to_le_bytes());
    data[8..].copy_from_slice(&op);
    events::emit(b"operator_set", &data);
    sdk::return_value(b"ok")
}

/// `remove_operator(index[8])` — refuses if the current occupant has any
/// active position.
fn do_remove_operator(args: &[u8]) -> i32 {
    if let Err(r) = require_owner() { return r; }
    let i = match read_u64(args, 0) { Some(v) => v, None => return sdk::return_value(b"err:invalid_args") };
    let key = op_key(i);
    let op = match read_32(&key) {
        Some(v) => v,
        None => return sdk::return_value(b"err:slot_empty"),
    };
    if has_active_position(&op) {
        return sdk::return_value(b"err:operator_has_position");
    }
    storage::set(&key, &[0u8; 32]);
    sdk::return_value(b"ok")
}

/// True iff `op` has any position the contract is tracking (delegated stake,
/// queued undelegate amount, or in-flight undelegations). Used by
/// `set_operator` and `set_op_count` to prevent stranding accounting outside
/// the active allowlist.
fn has_active_position(op: &[u8; 32]) -> bool {
    get_op_stake(op) > 0
        || get_pending_undelegate(op) > 0
        || get_inflight_undelegations(op) > 0
}

/// `set_op_count(count[8])` — change the active allowlist length.
///
/// **Refuses to shrink below any slot whose operator has an active
/// position.** F-03 audit fix: shrinking would hide that operator's stake
/// from `request_withdrawal` and `crank_undelegations`, stranding accounting.
fn do_admin_set_op_count(args: &[u8]) -> i32 {
    if let Err(r) = require_owner() { return r; }
    let n = match read_u64(args, 0) { Some(v) => v, None => return sdk::return_value(b"err:invalid_args") };
    if n > MAX_OPERATORS { return sdk::return_value(b"err:exceeds_max_operators"); }

    let current = get_op_count();
    if n < current {
        // Walk the slots that would be hidden and ensure none have active
        // positions.
        for i in n..current {
            let op = read_32(&op_key(i)).unwrap_or([0u8; 32]);
            if op != [0u8; 32] && has_active_position(&op) {
                return sdk::return_value(b"err:slot_in_range_has_position");
            }
        }
    }

    set_op_count(n);
    sdk::return_value(b"ok")
}

fn do_set_op_cap_bps(args: &[u8]) -> i32 {
    if let Err(r) = require_owner() { return r; }
    let b = match read_u64(args, 0) { Some(v) => v, None => return sdk::return_value(b"err:invalid_args") };
    if b > 10_000 { return sdk::return_value(b"err:bps_out_of_range"); }
    set_op_cap_bps(b);
    sdk::return_value(b"ok")
}

fn do_set_protocol_fee_bps(args: &[u8]) -> i32 {
    if let Err(r) = require_owner() { return r; }
    let b = match read_u64(args, 0) { Some(v) => v, None => return sdk::return_value(b"err:invalid_args") };
    if b > FEE_BPS_HARD_CAP { return sdk::return_value(b"err:fee_above_hard_cap"); }
    set_protocol_fee_bps(b);
    sdk::return_value(b"ok")
}

fn do_set_treasury(args: &[u8]) -> i32 {
    if let Err(r) = require_owner() { return r; }
    let t = match read_account(args, 0) { Some(a) => a, None => return sdk::return_value(b"err:invalid_args") };
    set_treasury(&t);
    sdk::return_value(b"ok")
}

fn do_set_slash_oracle(args: &[u8]) -> i32 {
    if let Err(r) = require_owner() { return r; }
    let o = match read_account(args, 0) { Some(a) => a, None => return sdk::return_value(b"err:invalid_args") };
    set_slash_oracle(&o);
    sdk::return_value(b"ok")
}

fn do_pause() -> i32 {
    if let Err(r) = require_owner() { return r; }
    set_paused(true);
    events::emit(b"paused", &[]);
    sdk::return_value(b"ok")
}

fn do_unpause() -> i32 {
    if let Err(r) = require_owner() { return r; }
    set_paused(false);
    events::emit(b"unpaused", &[]);
    sdk::return_value(b"ok")
}

// ── Reads ────────────────────────────────────────────────────────

/// Returns `(total_pooled_solen[16] || total_supply[16])`. Caller divides at
/// full precision rather than us doing it and losing bits.
fn do_exchange_rate() -> i32 {
    let mut out = [0u8; 32];
    out[..16].copy_from_slice(&get_total_pooled().to_le_bytes());
    out[16..].copy_from_slice(&get_total_supply().to_le_bytes());
    sdk::return_value(&out)
}

fn do_pending_undelegate_op_of(args: &[u8]) -> i32 {
    let op = match read_account(args, 0) { Some(a) => a, None => return sdk::return_value(b"err:invalid_args") };
    sdk::return_value(&get_pending_undelegate(&op).to_le_bytes())
}

fn do_op_stake_of(args: &[u8]) -> i32 {
    let op = match read_account(args, 0) { Some(a) => a, None => return sdk::return_value(b"err:invalid_args") };
    sdk::return_value(&get_op_stake(&op).to_le_bytes())
}

/// `withdrawal_at(seq[8])` — return the raw 56-byte queue entry layout:
/// `account[32] || solen_owed[16] || requested_epoch[8]`. Returns `b""`
/// (zero-length) when the entry has been tombstoned (already claimed) or
/// was never assigned.
fn do_withdrawal_at(args: &[u8]) -> i32 {
    let seq = match read_u64(args, 0) {
        Some(s) => s,
        None => return sdk::return_value(b"err:invalid_args"),
    };
    match read_wq(seq) {
        Some((account, solen_owed, requested_epoch)) => {
            let mut buf = [0u8; 56];
            buf[..32].copy_from_slice(&account);
            buf[32..48].copy_from_slice(&solen_owed.to_le_bytes());
            buf[48..56].copy_from_slice(&requested_epoch.to_le_bytes());
            sdk::return_value(&buf)
        }
        None => sdk::return_value(b""),
    }
}

/// `pending_withdrawals_of(account[32]) -> u64` — count of unclaimed queue
/// entries belonging to `account`. Linear in the active queue depth
/// (`wq_tail - wq_head`); intended for the dapp's Claims tab where it's
/// called once per page load.
fn do_pending_withdrawals_of(args: &[u8]) -> i32 {
    let target = match read_account(args, 0) {
        Some(a) => a,
        None => return sdk::return_value(b"err:invalid_args"),
    };
    let head = get_wq_head();
    let tail = get_wq_tail();
    let mut count: u64 = 0;
    let mut seq = head;
    while seq < tail {
        if let Some((account, _, _)) = read_wq(seq) {
            if account == target {
                count += 1;
            }
        }
        seq += 1;
    }
    sdk::return_value(&count.to_le_bytes())
}

