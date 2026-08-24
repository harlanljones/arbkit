//! Startup-time mapping between canonical outcomes and venue instruments.
//!
//! Catalog construction is intentionally allocation-friendly and lives outside
//! the engine hot loop. An instrument is active only after both venues have
//! supplied a mapping for opposite outcomes in the same binary market.

use std::collections::HashMap;

use arbkit_core::{MarketId, OutcomeId, VenueId};

use crate::alignment::{validate_binary_pair, OutcomeSide};
use crate::error::{MatchError, Result};
use arbkit_core::MarketKind;

/// A venue-specific identifier for one canonical outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VenueInstrument {
    /// Venue identifier.
    pub venue: VenueId,
    /// Canonical market identifier.
    pub market_id: MarketId,
    /// Canonical outcome identifier.
    pub outcome_id: OutcomeId,
    /// Kalshi ticker, when this is a Kalshi instrument.
    pub kalshi_ticker: Option<String>,
    /// Polymarket token id, when this is a Polymarket instrument.
    pub poly_token_id: Option<[u8; 32]>,
}

/// The two venue instruments that make up an active hedge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VenueInstrumentPair {
    /// Canonical market id.
    pub market_id: MarketId,
    /// First venue instrument.
    pub first: VenueInstrument,
    /// Second venue instrument.
    pub second: VenueInstrument,
}

/// Startup catalog of validated cross-venue binary pairs.
#[derive(Debug, Clone, Default)]
pub struct VenueInstrumentMap {
    active: HashMap<MarketId, VenueInstrumentPair>,
}

impl VenueInstrumentMap {
    /// Create an empty catalog.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a pair after validating market kind and complementary sides.
    pub fn register_binary_pair(
        &mut self,
        market_id: MarketId,
        kind: MarketKind,
        first: (VenueInstrument, OutcomeSide),
        second: (VenueInstrument, OutcomeSide),
    ) -> Result<()> {
        if first.0.market_id != market_id || second.0.market_id != market_id {
            return Err(MatchError::MalformedTicker(
                market_id.to_string(),
                "instrument market id does not match catalog key",
            ));
        }
        if first.0.venue == second.0.venue {
            return Err(MatchError::MalformedTicker(
                market_id.to_string(),
                "binary pair must use distinct venues",
            ));
        }
        validate_binary_pair(kind, first.1, kind, second.1)?;
        self.active.insert(
            market_id,
            VenueInstrumentPair {
                market_id,
                first: first.0,
                second: second.0,
            },
        );
        Ok(())
    }

    /// Return a validated active pair.
    pub fn get(&self, market_id: MarketId) -> Option<&VenueInstrumentPair> {
        self.active.get(&market_id)
    }

    /// Iterate active pairs in unspecified order.
    pub fn iter(&self) -> impl Iterator<Item = &VenueInstrumentPair> {
        self.active.values()
    }

    /// Number of active pairs.
    pub fn len(&self) -> usize {
        self.active.len()
    }

    /// Whether no active pairs exist.
    pub fn is_empty(&self) -> bool {
        self.active.is_empty()
    }
}

/// Parse a decimal Polymarket token id into the fixed-width wire form.
pub fn parse_poly_token_id(value: &str) -> Result<[u8; 32]> {
    let value = value.trim();
    if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
        return Err(MatchError::MalformedTicker(
            value.to_string(),
            "invalid Polymarket token id",
        ));
    }
    let mut out = [0u8; 32];
    for digit in value.bytes() {
        let mut carry = u16::from(digit - b'0');
        for byte in out.iter_mut().rev() {
            let next = u16::from(*byte) * 10 + carry;
            *byte = next as u8;
            carry = next / 256;
        }
        if carry != 0 {
            return Err(MatchError::MalformedTicker(
                value.to_string(),
                "token id is too large",
            ));
        }
    }
    Ok(out)
}

/// Render the fixed-width wire form back to the decimal string Gamma serves.
///
/// The catalog dump is for operators verifying mappings by hand against the
/// venue, so it must show exactly the identifiers Polymarket publishes.
/// Repeated division by 10 over big-endian bytes — integer-only, matching
/// [`parse_poly_token_id`], which this round-trips.
pub fn poly_token_id_to_decimal(token: &[u8; 32]) -> String {
    // Copy into 64-bit limbs (big-endian) so division works in chunks.
    let mut limbs = [0u64; 4];
    for (i, limb) in limbs.iter_mut().enumerate() {
        let bytes: [u8; 8] = token[i * 8..(i + 1) * 8].try_into().unwrap();
        *limb = u64::from_be_bytes(bytes);
    }

    // Repeated division by 1e19 yields base-1e19 digits, lowest first. The
    // most significant chunk prints without padding.
    const DIVISOR: u64 = 10_000_000_000_000_000_000;
    let mut chunks: Vec<String> = Vec::with_capacity(5);
    loop {
        let mut rem = 0u64;
        for limb in limbs.iter_mut() {
            let cur = (u128::from(rem) << 64) | u128::from(*limb);
            *limb = (cur / u128::from(DIVISOR)) as u64;
            rem = (cur % u128::from(DIVISOR)) as u64;
        }
        if limbs.iter().all(|&l| l == 0) {
            if rem == 0 && chunks.is_empty() {
                chunks.push("0".to_string());
            } else {
                chunks.push(rem.to_string());
            }
            break;
        }
        chunks.push(format!("{rem:019}"));
    }

    chunks.reverse();
    chunks.concat()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::alignment::OutcomeSide;
    use arbkit_core::MarketKind;

    #[test]
    fn token_decimal_round_trips_through_wire_form() {
        // A real Gamma token id captured in the feed fixtures.
        let decimal =
            "52222696630142282340910360142526626600364417636157306499739927226918483751010";
        let wire = parse_poly_token_id(decimal).unwrap();
        assert_eq!(poly_token_id_to_decimal(&wire), decimal);

        // Zero and small values render without padding artifacts.
        assert_eq!(poly_token_id_to_decimal(&[0u8; 32]), "0");
        let small = parse_poly_token_id("42").unwrap();
        assert_eq!(poly_token_id_to_decimal(&small), "42");

        // The maximum u256 renders exactly.
        let max = [0xffu8; 32];
        let rendered = poly_token_id_to_decimal(&max);
        assert_eq!(parse_poly_token_id(&rendered).unwrap(), max);
    }

    fn instrument(venue: VenueId, outcome_id: OutcomeId) -> VenueInstrument {
        VenueInstrument {
            venue,
            market_id: 7,
            outcome_id,
            kalshi_ticker: None,
            poly_token_id: None,
        }
    }

    #[test]
    fn token_ids_are_parsed_without_floating_point() {
        assert_eq!(parse_poly_token_id("0").unwrap()[31], 0);
        assert_eq!(parse_poly_token_id("255").unwrap()[31], 255);
        assert_eq!(parse_poly_token_id("256").unwrap()[30..], [1, 0]);
        assert!(parse_poly_token_id("not-a-token").is_err());
    }

    #[test]
    fn only_complementary_cross_venue_pairs_activate() {
        let mut map = VenueInstrumentMap::new();
        assert!(map
            .register_binary_pair(
                7,
                MarketKind::Moneyline,
                (instrument(1, 1), OutcomeSide::Home),
                (instrument(2, 2), OutcomeSide::Away)
            )
            .is_ok());
        assert_eq!(map.len(), 1);
        assert!(map
            .register_binary_pair(
                8,
                MarketKind::Moneyline,
                (instrument(1, 1), OutcomeSide::Home),
                (instrument(2, 2), OutcomeSide::Home)
            )
            .is_err());
        assert_eq!(map.len(), 1);
    }
}
