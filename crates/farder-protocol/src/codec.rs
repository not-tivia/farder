use anyhow::Result;
use serde::{Deserialize, Serialize};

pub fn encode<T: Serialize>(msg: &T) -> Result<Vec<u8>> {
    rmp_serde::to_vec(msg).map_err(Into::into)
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
