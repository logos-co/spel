//! Hex encoding/decoding utilities.

pub fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

/// # Errors
///
/// Returns an error if `hex` has an odd length or contains non-hex characters.
pub fn hex_decode(hex: &str) -> Result<Vec<u8>, String> {
    if !hex.len().is_multiple_of(2) {
        return Err(format!("Hex string has odd length: {}", hex.len()));
    }
    let mut bytes = Vec::with_capacity(hex.len() / 2);
    for i in (0..hex.len()).step_by(2) {
        let byte = u8::from_str_radix(&hex[i..i + 2], 16)
            .map_err(|e| format!("Invalid hex at position {}: {}", i, e))?;
        bytes.push(byte);
    }
    Ok(bytes)
}

/// Decode a 32-byte value from base58 or hex string.
/// Strips "Public/" or "Private/" prefix if present before decoding.
///
/// # Errors
///
/// Returns an error if `input` is not a valid 32-byte base58 or hex value.
pub fn decode_bytes_32(input: &str) -> Result<[u8; 32], String> {
    spel_framework_core::pda::parse_bytes32(input)
}

/// Parse an account ID, returning the decoded bytes and whether it had a "Private/" prefix.
///
/// # Errors
///
/// Returns an error if `input` is not a valid 32-byte base58 or hex value.
pub fn parse_account_id(input: &str) -> Result<([u8; 32], bool), String> {
    let is_private = input.starts_with("Private/");
    let bytes = decode_bytes_32(input)?;
    Ok((bytes, is_private))
}

/// Split 32 bytes into eight little-endian `u32` words.
///
/// Used to build risc0-style `[u32; 8]` program IDs from decoded byte
/// strings. `chunks_exact(4)` over a fixed 32-byte input always yields
/// exactly 8 chunks of exactly 4 bytes, so the conversion cannot fail.
///
/// # Panics
///
/// Does not panic: see above.
pub fn bytes32_to_u32_words(bytes: [u8; 32]) -> [u32; 8] {
    let mut words = [0u32; 8];
    for (word, chunk) in words.iter_mut().zip(bytes.chunks_exact(4)) {
        #[allow(clippy::unwrap_used)]
        {
            *word = u32::from_le_bytes(chunk.try_into().unwrap());
        }
    }
    words
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_hex() -> String {
        // Use 0x prefix to force hex (not base58) decoding
        format!("0x{}", "ab".repeat(32))
    }

    #[test]
    fn test_parse_account_id_not_private() {
        let (bytes, is_priv) = parse_account_id(&test_hex()).unwrap();
        assert_eq!(bytes, [0xab; 32]);
        assert!(!is_priv);
    }

    #[test]
    fn test_parse_account_id_private_prefix_hex() {
        let input = format!("Private/{}", test_hex());
        let (bytes, is_priv) = parse_account_id(&input).unwrap();
        assert_eq!(bytes, [0xab; 32]);
        assert!(is_priv, "Private/ prefix should set is_priv=true");
    }

    #[test]
    fn test_parse_account_id_public_prefix_not_private() {
        let input = format!("Public/{}", test_hex());
        let (_, is_priv) = parse_account_id(&input).unwrap();
        assert!(!is_priv, "Public/ prefix should not set is_priv");
    }

    #[test]
    fn test_decode_bytes_32_strips_private_prefix() {
        let with_prefix = format!("Private/{}", test_hex());
        let without = decode_bytes_32(&with_prefix).unwrap();
        let direct = decode_bytes_32(&test_hex()).unwrap();
        assert_eq!(without, direct);
    }

    #[test]
    fn test_parse_account_id_private_prefix_0x() {
        let hex = format!("0x{}", "cd".repeat(32));
        let input = format!("Private/{}", hex);
        let (bytes, is_priv) = parse_account_id(&input).unwrap();
        assert_eq!(bytes, [0xcd; 32]);
        assert!(is_priv);
    }
}
