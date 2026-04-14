//! Hex encoding/decoding utilities.

use base58::FromBase58;

pub fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

pub fn hex_decode(hex: &str) -> Result<Vec<u8>, String> {
    if hex.len() % 2 != 0 {
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
pub fn decode_bytes_32(input: &str) -> Result<[u8; 32], String> {
    let input = input
        .strip_prefix("Public/")
        .or_else(|| input.strip_prefix("Private/"))
        .unwrap_or(input);

    if let Ok(bytes) = input.from_base58() {
        if bytes.len() == 32 {
            let mut arr = [0u8; 32];
            arr.copy_from_slice(&bytes);
            return Ok(arr);
        }
        return Err(format!(
            "Base58 decoded to {} bytes, expected 32",
            bytes.len()
        ));
    }

    let hex = input
        .strip_prefix("0x")
        .or_else(|| input.strip_prefix("0X"))
        .unwrap_or(input);
    let bytes = hex_decode(hex)?;
    if bytes.len() == 32 {
        let mut arr = [0u8; 32];
        arr.copy_from_slice(&bytes);
        Ok(arr)
    } else {
        Err(format!(
            "Expected 32 bytes, got {} (provide base58 or 64 hex chars)",
            bytes.len()
        ))
    }
}

/// Parse an account ID, returning the decoded bytes and whether it had a "Private/" prefix.
pub fn parse_account_id(input: &str) -> Result<([u8; 32], bool), String> {
    let is_private = input.starts_with("Private/");
    let bytes = decode_bytes_32(input)?;
    Ok((bytes, is_private))
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
