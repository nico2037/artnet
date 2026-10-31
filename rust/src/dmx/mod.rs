//! DMX input: Art-Net reception and universe numbering. Resolving DMX to fixture attributes follows in `resolve`.

pub mod artnet;

/// Universe numbering between MVR (1-based, `Addresses/Address`) and Art-Net (15-bit Port-Address, 0-based).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UniverseMap {
    /// MVR universe that is received on Art-Net Port-Address 0.
    pub first_mvr_universe: i32,
}

impl Default for UniverseMap {
    fn default() -> Self {
        Self { first_mvr_universe: 1 }
    }
}

impl UniverseMap {
    pub const MAX_PORT_ADDRESS: i32 = 0x7fff;

    /// Art-Net Port-Address for an MVR universe, `None` if it falls outside 0..=32767.
    pub fn to_artnet(self, mvr_universe: i32) -> Option<u16> {
        let port = mvr_universe - self.first_mvr_universe;
        (0..=Self::MAX_PORT_ADDRESS).contains(&port).then_some(port as u16)
    }

    pub fn to_mvr(self, port_address: u16) -> i32 {
        port_address as i32 + self.first_mvr_universe
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_maps_mvr_universe_1_to_artnet_0() {
        let map = UniverseMap::default();
        assert_eq!(map.to_artnet(1), Some(0));
        assert_eq!(map.to_artnet(17), Some(16));
        assert_eq!(map.to_artnet(0), None);
        assert_eq!(map.to_mvr(0), 1);
    }

    #[test]
    fn offset_shifts_both_directions_and_clamps_to_15_bit() {
        let map = UniverseMap { first_mvr_universe: 0 };
        assert_eq!(map.to_artnet(0), Some(0));
        assert_eq!(map.to_artnet(32_767), Some(32_767));
        assert_eq!(map.to_artnet(32_768), None);
        let map = UniverseMap { first_mvr_universe: -4 };
        assert_eq!(map.to_artnet(1), Some(5));
        assert_eq!(map.to_mvr(5), 1);
    }
}
