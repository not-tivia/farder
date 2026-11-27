use anyhow::Result;
use serde::{Deserialize, Serialize};

/// Encode a wire value. Structs go out as NAMED maps (`to_vec_named`) so an older
/// reader ignores fields it does not know. `decode` accepts both named and
/// positional forms. Do NOT use this for signed or hashed bytes (those stay
/// positional via `rmp_serde::to_vec` in `farder-crypto`).
pub fn encode<T: Serialize>(msg: &T) -> Result<Vec<u8>> {
    rmp_serde::to_vec_named(msg).map_err(Into::into)
}

pub fn decode<'a, T: Deserialize<'a>>(data: &'a [u8]) -> Result<T> {
    rmp_serde::from_slice(data).map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_roundtrip_string() {
        let original = String::from("hello farder");
        let encoded = encode(&original).expect("encode failed");
        let decoded: String = decode(&encoded).expect("decode failed");
        assert_eq!(original, decoded);
    }
}
