//! Fee accounting: gas pricing, fee deduction, treasury crediting.
//!
//! Solen uses a simple base-fee model. Each block has a base fee per gas unit.
//! Fees are deducted from the sender's balance after execution and credited
//! to the treasury account.

use solen_types::AccountId;

/// Fee configuration for the execution engine.
#[derive(Debug, Clone)]
pub struct FeeConfig {
    /// Base fee per gas unit (in smallest token units).
    pub base_fee_per_gas: u128,
    /// Treasury account that receives fees.
    pub treasury_account: AccountId,
    /// Fraction of fees burned (basis points, out of 10_000).
    /// The remainder goes to the treasury.
    pub burn_rate_bps: u64,
}

impl Default for FeeConfig {
    fn default() -> Self {
        let mut treasury = [0u8; 32];
        treasury[..8].copy_from_slice(b"treasury");
        Self {
            base_fee_per_gas: 1,
            treasury_account: treasury,
            burn_rate_bps: 5000, // 50% burned, 50% to treasury
        }
    }
}

impl FeeConfig {
    /// Calculate the total fee for a given gas amount.
    pub fn calculate_fee(&self, gas_used: u64) -> u128 {
        self.base_fee_per_gas * gas_used as u128
    }

    /// Calculate the treasury portion (not burned).
    pub fn treasury_amount(&self, total_fee: u128) -> u128 {
        total_fee * (10_000 - self.burn_rate_bps) as u128 / 10_000
    }

    /// Calculate the burned portion.
    pub fn burn_amount(&self, total_fee: u128) -> u128 {
        total_fee * self.burn_rate_bps as u128 / 10_000
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fee_calculation() {
        let config = FeeConfig {
            base_fee_per_gas: 10,
            burn_rate_bps: 5000,
            ..Default::default()
        };

        assert_eq!(config.calculate_fee(100), 1000);
        assert_eq!(config.treasury_amount(1000), 500);
        assert_eq!(config.burn_amount(1000), 500);
    }

    #[test]
    fn zero_burn() {
        let config = FeeConfig {
            base_fee_per_gas: 5,
            burn_rate_bps: 0,
            ..Default::default()
        };

        let fee = config.calculate_fee(200);
        assert_eq!(fee, 1000);
        assert_eq!(config.treasury_amount(fee), 1000);
        assert_eq!(config.burn_amount(fee), 0);
    }
}
