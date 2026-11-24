//! Spatial keys for the linear bounding volume hierarchy.

/// Quantizes each coordinate to ten bits within the bounds and interleaves
/// them, with X in bit 0, Y in bit 1, and Z in bit 2 of each group.
/// Coordinates outside the bounds clamp to the endpoints. A collapsed axis
/// contributes zero bits.
pub fn morton_3d(x: f64, y: f64, z: f64, bbox_min: [f64; 3], bbox_max: [f64; 3]) -> u32 {
    let coords = [x, y, z];
    let bits: [u32; 3] = std::array::from_fn(|axis| {
        let min = bbox_min[axis];
        let max = bbox_max[axis];
        if max <= min {
            return 0;
        }
        let extent = max - min;
        let fraction = if extent.is_finite() {
            (coords[axis] - min) / extent
        } else {
            // Halve before subtracting when distinct finite bounds span more
            // than f64::MAX.
            (coords[axis] * 0.5 - min * 0.5) / (max * 0.5 - min * 0.5)
        };
        spread_10_bits(((fraction * 1024.0).clamp(0.0, 1023.0)) as u32)
    });
    bits[0] | (bits[1] << 1) | (bits[2] << 2)
}

fn spread_10_bits(mut value: u32) -> u32 {
    value = (value | (value << 16)) & 0x0300_00ff;
    value = (value | (value << 8)) & 0x0300_f00f;
    value = (value | (value << 4)) & 0x030c_30c3;
    (value | (value << 2)) & 0x0924_9249
}
