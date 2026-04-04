//! Address encoding: Base58 (Bitcoin alphabet) and hex utilities.
//!
//! Account IDs are displayed as Base58 for shorter, human-friendly addresses.
//! All inputs accept both Base58 and hex formats, auto-detected by length and character set.

const BASE58_ALPHABET: &[u8] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";

/// Encode bytes as Base58 (Bitcoin alphabet, no checksum).
pub fn base58_encode(bytes: &[u8]) -> String {
    if bytes.is_empty() {
        return String::new();
    }

    // Count leading zeros.
    let zeros = bytes.iter().take_while(|&&b| b == 0).count();

    // Convert to base58 using big-number division.
    let mut digits: Vec<u8> = Vec::with_capacity(bytes.len() * 138 / 100 + 1);
    for &byte in bytes {
        let mut carry = byte as u32;
        for d in digits.iter_mut() {
            carry += (*d as u32) * 256;
            *d = (carry % 58) as u8;
            carry /= 58;
        }
        while carry > 0 {
            digits.push((carry % 58) as u8);
            carry /= 58;
        }
    }

    let mut result = String::with_capacity(zeros + digits.len());
    // Leading '1's for each leading zero byte.
    for _ in 0..zeros {
        result.push('1');
    }
    // Digits in reverse order.
    for &d in digits.iter().rev() {
        result.push(BASE58_ALPHABET[d as usize] as char);
    }
    result
}

/// Decode a Base58 string to bytes.
pub fn base58_decode(s: &str) -> Result<Vec<u8>, String> {
    if s.is_empty() {
        return Ok(Vec::new());
    }

    // Build reverse lookup table.
    let mut table = [255u8; 128];
    for (i, &c) in BASE58_ALPHABET.iter().enumerate() {
        table[c as usize] = i as u8;
    }

    // Count leading '1's (zeros).
    let zeros = s.bytes().take_while(|&b| b == b'1').count();

    // Convert from base58.
    let mut bytes: Vec<u8> = Vec::with_capacity(s.len() * 733 / 1000 + 1);
    for c in s.bytes() {
        if c >= 128 {
            return Err(format!("invalid base58 character: {}", c as char));
        }
        let val = table[c as usize];
        if val == 255 {
            return Err(format!("invalid base58 character: {}", c as char));
        }
        let mut carry = val as u32;
        for b in bytes.iter_mut() {
            carry += (*b as u32) * 58;
            *b = (carry & 0xFF) as u8;
            carry >>= 8;
        }
        while carry > 0 {
            bytes.push((carry & 0xFF) as u8);
            carry >>= 8;
        }
    }

    let mut result = Vec::with_capacity(zeros + bytes.len());
    for _ in 0..zeros {
        result.push(0);
    }
    result.extend(bytes.into_iter().rev());
    Ok(result)
}

/// Encode a 32-byte AccountId/ValidatorId as Base58.
pub fn account_to_base58(id: &[u8; 32]) -> String {
    base58_encode(id)
}

/// Hex-encode bytes (lowercase).
pub fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

/// Hex-decode a string (with optional 0x prefix).
pub fn hex_decode(s: &str) -> Result<Vec<u8>, String> {
    let s = s.strip_prefix("0x").unwrap_or(s);
    if s.len() % 2 != 0 {
        return Err("hex string must have even length".into());
    }
    (0..s.len())
        .step_by(2)
        .map(|i| {
            u8::from_str_radix(&s[i..i + 2], 16)
                .map_err(|_| format!("invalid hex at position {}", i))
        })
        .collect()
}

/// Parse an address string that could be either Base58 or hex.
/// Returns the 32-byte AccountId.
pub fn parse_address(s: &str) -> Result<[u8; 32], String> {
    let s = s.trim();
    let clean = s.strip_prefix("0x").unwrap_or(s);

    // Try hex first: exactly 64 hex characters.
    if clean.len() == 64 && clean.chars().all(|c| c.is_ascii_hexdigit()) {
        let bytes = hex_decode(clean)?;
        let mut id = [0u8; 32];
        id.copy_from_slice(&bytes);
        return Ok(id);
    }

    // Try Base58: typical length is 43-44 characters for 32 bytes.
    let bytes = base58_decode(s).map_err(|e| format!("invalid address: {e}"))?;
    if bytes.len() != 32 {
        return Err(format!(
            "address must be 32 bytes, got {} (from Base58)",
            bytes.len()
        ));
    }
    let mut id = [0u8; 32];
    id.copy_from_slice(&bytes);
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base58_roundtrip() {
        let input = [0x01u8; 32];
        let encoded = base58_encode(&input);
        let decoded = base58_decode(&encoded).unwrap();
        assert_eq!(decoded, input);
    }

    #[test]
    fn base58_leading_zeros() {
        let mut input = [0u8; 32];
        input[31] = 1;
        let encoded = base58_encode(&input);
        assert!(encoded.starts_with('1'), "leading zeros should produce leading '1's");
        let decoded = base58_decode(&encoded).unwrap();
        assert_eq!(decoded, input);
    }

    #[test]
    fn base58_all_zeros() {
        let input = [0u8; 32];
        let encoded = base58_encode(&input);
        assert_eq!(encoded, "11111111111111111111111111111111"); // 32 '1's
        let decoded = base58_decode(&encoded).unwrap();
        assert_eq!(decoded, input);
    }

    #[test]
    fn parse_address_hex() {
        let id = [0xAB; 32];
        let hex = hex_encode(&id);
        let parsed = parse_address(&hex).unwrap();
        assert_eq!(parsed, id);
    }

    #[test]
    fn parse_address_hex_with_prefix() {
        let id = [0xAB; 32];
        let hex = format!("0x{}", hex_encode(&id));
        let parsed = parse_address(&hex).unwrap();
        assert_eq!(parsed, id);
    }

    #[test]
    fn parse_address_base58() {
        let id = [0xAB; 32];
        let b58 = base58_encode(&id);
        let parsed = parse_address(&b58).unwrap();
        assert_eq!(parsed, id);
    }

    #[test]
    fn parse_address_rejects_wrong_length() {
        assert!(parse_address("abc").is_err());
        assert!(parse_address("").is_err());
    }

    #[test]
    fn base58_known_vector() {
        // Single byte 0x00 should encode to "1"
        assert_eq!(base58_encode(&[0]), "1");
        // Single byte 0x01 should encode to "2"
        assert_eq!(base58_encode(&[1]), "2");
    }

    #[test]
    fn hex_roundtrip() {
        let input = [0xDE, 0xAD, 0xBE, 0xEF];
        let encoded = hex_encode(&input);
        assert_eq!(encoded, "deadbeef");
        let decoded = hex_decode(&encoded).unwrap();
        assert_eq!(decoded, input);
    }
}
