//! serde `with` helpers for `Box<[T; N]>` fields (heap-allocated fixed arrays
//! that serde's derive cannot handle directly: it has no `Serialize`/`Deserialize`
//! impls for arrays longer than 32, and none for boxed arrays at all). Each
//! helper serializes the array as a length-prefixed sequence and rebuilds the
//! exact-size boxed array on load, rejecting a wrong length.

pub mod boxed_bytes {
    use serde::de::Error as _;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S, const N: usize>(v: &[u8; N], s: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        v[..].serialize(s)
    }

    pub fn deserialize<'de, D, const N: usize>(d: D) -> Result<Box<[u8; N]>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let v = Vec::<u8>::deserialize(d)?;
        let boxed: Box<[u8]> = v.into_boxed_slice();
        boxed
            .try_into()
            .map_err(|b: Box<[u8]>| D::Error::custom(format!("expected {N} bytes, got {}", b.len())))
    }
}

pub mod boxed_words {
    use serde::de::Error as _;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S, const N: usize>(v: &[u16; N], s: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        v[..].serialize(s)
    }

    pub fn deserialize<'de, D, const N: usize>(d: D) -> Result<Box<[u16; N]>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let v = Vec::<u16>::deserialize(d)?;
        let boxed: Box<[u16]> = v.into_boxed_slice();
        boxed
            .try_into()
            .map_err(|b: Box<[u16]>| D::Error::custom(format!("expected {N} words, got {}", b.len())))
    }
}
