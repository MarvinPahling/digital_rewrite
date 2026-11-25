//! Width-aware digital signal values. High-impedance bits are represented separately from
//! value bits; construction canonicalizes the latter to zero beneath the impedance mask.

use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SignalWidth(u8);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SignalWidthError(pub u8);

impl SignalWidth {
    pub const MIN: u8 = 1;
    pub const MAX: u8 = 64;
    pub const ONE: Self = Self(1);

    pub fn new(width: u8) -> Result<Self, SignalWidthError> {
        (Self::MIN..=Self::MAX)
            .contains(&width)
            .then_some(Self(width))
            .ok_or(SignalWidthError(width))
    }

    pub const fn get(self) -> u8 {
        self.0
    }

    pub const fn mask(self) -> u64 {
        if self.0 == 64 {
            u64::MAX
        } else {
            (1u64 << self.0) - 1
        }
    }
}

impl fmt::Display for SignalWidthError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "signal width must be 1..=64 (got {})", self.0)
    }
}
impl std::error::Error for SignalWidthError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SignalValue {
    width: SignalWidth,
    bits: u64,
    high_z: u64,
}

/// Explicit policy for reading high-impedance positions when interpreting signed values.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SamplingPolicy {
    HighZIsZero,
    RandomBits(u64),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SignalValueError {
    NotOneBit,
    HighImpedance,
    WidthMismatch,
}

impl SignalValue {
    pub const fn new(width: SignalWidth, bits: u64, high_z: u64) -> Self {
        let mask = width.mask();
        let high_z = high_z & mask;
        Self {
            width,
            bits: bits & mask & !high_z,
            high_z,
        }
    }

    pub const fn known(width: SignalWidth, bits: u64) -> Self {
        Self::new(width, bits, 0)
    }

    pub const fn width(self) -> SignalWidth {
        self.width
    }
    pub const fn bits(self) -> u64 {
        self.bits
    }
    pub const fn high_z(self) -> u64 {
        self.high_z
    }
    pub const fn is_fully_known(self) -> bool {
        self.high_z == 0
    }

    /// Treats every high-Z bit as zero; this operation consumes no randomness.
    pub const fn read_zero(self) -> u64 {
        self.bits
    }

    /// Resolves high-Z bits using caller-supplied random bits, sampled only at those positions.
    pub const fn read_random(self, random_bits: u64) -> u64 {
        self.bits | (random_bits & self.high_z)
    }

    pub const fn unsigned(self) -> u64 {
        self.bits
    }

    /// Two's-complement interpretation using the caller's explicit high-Z sampling policy.
    pub const fn signed_with(self, policy: SamplingPolicy) -> i64 {
        let bits = match policy {
            SamplingPolicy::HighZIsZero => self.read_zero(),
            SamplingPolicy::RandomBits(random) => self.read_random(random),
        };
        let sign = 1u64 << (self.width.get() - 1);
        if self.width.get() == 64 {
            bits as i64
        } else if bits & sign != 0 {
            (bits | !self.width.mask()) as i64
        } else {
            bits as i64
        }
    }

    pub fn as_bool(self) -> Result<bool, SignalValueError> {
        if self.width.get() != 1 {
            return Err(SignalValueError::NotOneBit);
        }
        if self.high_z != 0 {
            return Err(SignalValueError::HighImpedance);
        }
        Ok(self.bits != 0)
    }

    /// Bitwise AND for equal-width, fully-known operands. High-Z sampling is deliberately a
    /// separate operation and is never hidden inside gate evaluation.
    pub fn bitwise_and(self, rhs: Self) -> Result<Self, SignalValueError> {
        self.binary_known(rhs, |a, b| a & b)
    }

    pub fn bitwise_or(self, rhs: Self) -> Result<Self, SignalValueError> {
        self.binary_known(rhs, |a, b| a | b)
    }

    pub fn bitwise_xor(self, rhs: Self) -> Result<Self, SignalValueError> {
        self.binary_known(rhs, |a, b| a ^ b)
    }

    pub fn bitwise_not(self) -> Result<Self, SignalValueError> {
        if self.high_z != 0 {
            return Err(SignalValueError::HighImpedance);
        }
        Ok(Self::known(self.width, !self.bits))
    }

    fn binary_known(
        self,
        rhs: Self,
        operation: impl FnOnce(u64, u64) -> u64,
    ) -> Result<Self, SignalValueError> {
        if self.width != rhs.width {
            return Err(SignalValueError::WidthMismatch);
        }
        if self.high_z != 0 || rhs.high_z != 0 {
            return Err(SignalValueError::HighImpedance);
        }
        Ok(Self::known(self.width, operation(self.bits, rhs.bits)))
    }

    pub fn format_hex(self) -> String {
        format!("0x{:X}", self.bits)
    }
    pub fn format_unsigned(self) -> String {
        self.bits.to_string()
    }
    /// Signed decimal formatting requires a sampling choice for any high-Z bits.
    pub fn format_signed(self, policy: SamplingPolicy) -> String {
        self.signed_with(policy).to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn widths_and_masks_cover_the_entire_supported_range() {
        assert_eq!(SignalWidth::new(0), Err(SignalWidthError(0)));
        assert_eq!(SignalWidth::new(65), Err(SignalWidthError(65)));
        assert_eq!(SignalWidth::new(1).unwrap().mask(), 1);
        assert_eq!(SignalWidth::new(64).unwrap().mask(), u64::MAX);
        for width in 1..=64 {
            let width = SignalWidth::new(width).unwrap();
            let value = SignalValue::new(width, u64::MAX, u64::MAX);
            assert_eq!(value.bits(), 0);
            assert_eq!(value.high_z(), width.mask());
            let value = SignalValue::new(width, u64::MAX, 0);
            assert_eq!(value.bits(), width.mask());
        }
    }

    #[test]
    fn canonicalization_equality_and_explicit_high_z_reads() {
        let width = SignalWidth::new(4).unwrap();
        let a = SignalValue::new(width, 0b1111, 0b1010);
        let b = SignalValue::new(width, 0b0101, 0b1010);
        assert_eq!(a, b);
        assert_ne!(a, SignalValue::new(width, 0b0101, 0b1000));
        assert_eq!(a.read_zero(), 0b0101);
        assert_eq!(a.read_random(0b1010), 0b1111);
        assert_eq!(a.read_random(0), 0b0101);
    }

    #[test]
    fn signed_boundaries_and_checked_boolean_conversion() {
        for (width, bits, signed) in [
            (1, 1, -1),
            (1, 0, 0),
            (63, 1u64 << 62, -(1i64 << 62)),
            (64, 1u64 << 63, i64::MIN),
        ] {
            assert_eq!(
                SignalValue::known(SignalWidth::new(width).unwrap(), bits)
                    .signed_with(SamplingPolicy::HighZIsZero),
                signed
            );
        }
        let one = SignalWidth::new(1).unwrap();
        assert_eq!(SignalValue::known(one, 1).as_bool(), Ok(true));
        assert_eq!(
            SignalValue::new(one, 1, 1).as_bool(),
            Err(SignalValueError::HighImpedance)
        );
        assert_eq!(
            SignalValue::known(SignalWidth::new(2).unwrap(), 1).as_bool(),
            Err(SignalValueError::NotOneBit)
        );
        let value = SignalValue::known(SignalWidth::new(8).unwrap(), 0xA5);
        assert_eq!(value.format_hex(), "0xA5");
        assert_eq!(value.format_unsigned(), "165");
        assert_eq!(value.format_signed(SamplingPolicy::HighZIsZero), "-91");

        let high_z_sign = SignalValue::new(SignalWidth::new(4).unwrap(), 0, 0b1000);
        assert_eq!(high_z_sign.format_signed(SamplingPolicy::HighZIsZero), "0");
        assert_eq!(
            high_z_sign.format_signed(SamplingPolicy::RandomBits(0b1000)),
            "-8"
        );
        assert_eq!(high_z_sign.format_hex(), "0x0");
        assert_eq!(high_z_sign.format_unsigned(), "0");
    }
}
