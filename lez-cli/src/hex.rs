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
pub fn decode_bytes_32(input: &str) -> Result<[u8; 32], String> {
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
