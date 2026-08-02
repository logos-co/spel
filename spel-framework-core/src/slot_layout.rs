//! Layout arithmetic for embedded slots. The `account_type` macro emits
//! offset consts as sums of `FixedBorshSize::SIZE` terms, so rustc
//! evaluates the number and type aliases resolve. A type without an
//! impl cannot precede a slot field, which enforces the fixed-size
//! prefix rule at compile time.

use nssa_core::account::AccountId;

/// Borsh-serialized size of a type that is the same for every value.
pub trait FixedBorshSize {
    const SIZE: usize;
}

macro_rules! impl_fixed {
    ($($t:ty => $n:expr),* $(,)?) => {
        $(impl FixedBorshSize for $t { const SIZE: usize = $n; })*
    };
}

impl_fixed! {
    u8 => 1, u16 => 2, u32 => 4, u64 => 8, u128 => 16,
    i8 => 1, i16 => 2, i32 => 4, i64 => 8, i128 => 16,
    bool => 1,
}

impl<T: FixedBorshSize, const N: usize> FixedBorshSize for [T; N] {
    const SIZE: usize = T::SIZE * N;
}

// Foreign fixed-size types consumers commonly embed before a slot.
// The orphan rule puts these impls here, next to hte trait: nobody
// downstream own either side.
impl FixedBorshSize for AccountId {
    const SIZE: usize = 32;
}

/// A recognizable non-zero value of a slot type, used by the emitted
/// layout test: serialize an instance with the probe in the slot field
/// and assert the probe bytes sit at the derived offset. All-zeros
/// would be indistinguishable from default-initialized neighbors.
pub trait SlotLayoutProbe: Sized {
    fn probe() -> Self;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn primitive_and_array_sizes_compose() {
        assert_eq!(<u64 as FixedBorshSize>::SIZE, 8);
        assert_eq!(<[u8; 24] as FixedBorshSize>::SIZE, 24);
        assert_eq!(<[u32; 4] as FixedBorshSize>::SIZE, 16);
        assert_eq!(<AccountId as FixedBorshSize>::SIZE, 32);
    }
}
