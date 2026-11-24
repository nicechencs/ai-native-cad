//! Direct implicit-field sampling and layer contour extraction.

use std::collections::HashMap;

use crate::{SliceField, SlicerError};

const MAX_GRID_CELLS: usize = 20_000_000;
const MAX_LAYERS: usize = 100_000;

/// Parameters controlling the XY grid, layer slabs and contour stitching.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SliceConfig {
    pub bbox_min_mm: [f64; 3],
    pub bbox_max_mm: [f64; 3],
    pub layer_height_mm: f64,
    pub xy_step_mm: f64,
    /// Minimum spacing of consecutive output vertices on a stitched path.
    pub stitch_tol_mm: f64,
}

/// A stitched contour. Closed contours have an implicit last-to-first edge.
#[derive(Clone, Debug, PartialEq)]
pub struct ContourLoop {
    pub points_mm: Vec<[f64; 2]>,
    /// Positive for CCW outer contours, negative for CW holes and zero for open paths.
    pub signed_area_mm2: f64,
    pub is_hole: bool,
    pub closed: bool,
}

/// Contours and measurements for one layer.
#[derive(Clone, Debug, PartialEq)]
pub struct SliceLayer {
    pub layer_index: usize,
    /// Upper surface of the slab. The field is sampled at the slab midpoint.
    pub z_mm: f64,
    pub loops: Vec<ContourLoop>,
    pub net_area_mm2: f64,
    pub total_perimeter_mm: f64,
}

/// The layers extracted from an implicit field.
#[derive(Clone, Debug, PartialEq)]
pub struct SliceStack {
    pub config: SliceConfig,
    pub layers: Vec<SliceLayer>,
}

/// Samples each slab at its midpoint to avoid coinciding with horizontal caps.
/// A final partial slab ends at the upper Z bound. Contours that leave the XY
/// bounds are returned as open inspection paths.
pub fn slice_field(
    field: &(impl SliceField + ?Sized),
    config: &SliceConfig,
) -> Result<SliceStack, SlicerError> {
    validate_config(config)?;
    let [min_x, min_y, min_z] = config.bbox_min_mm;
    let [max_x, max_y, max_z] = config.bbox_max_mm;
    let x_cells = cell_count(max_x - min_x, config.xy_step_mm, MAX_GRID_CELLS)?;
    let y_cells = cell_count(max_y - min_y, config.xy_step_mm, MAX_GRID_CELLS)?;
    if x_cells > MAX_GRID_CELLS / y_cells {
        return Err(SlicerError::InvalidConfig("XY grid is too large"));
    }
    let layer_count = cell_count(max_z - min_z, config.layer_height_mm, MAX_LAYERS)?;
    let grid = Grid {
        xs: axis_coordinates(min_x, max_x, x_cells)?,
        ys: axis_coordinates(min_y, max_y, y_cells)?,
    };
    let mut values = Vec::with_capacity(grid.xs.len() * grid.ys.len());
    let mut layers = Vec::with_capacity(layer_count);
    let mut bottom = min_z;
    for layer_index in 0..layer_count {
        let z_mm = if layer_index + 1 == layer_count {
            max_z
        } else {
            ((layer_index + 1) as f64)
                .mul_add(config.layer_height_mm, min_z)
                .min(max_z)
        };
        if z_mm <= bottom {
            return Err(SlicerError::InvalidConfig(
                "layer height is below coordinate precision",
            ));
        }
        let sample_z = bottom.midpoint(z_mm);
        values.clear();
        for &y in &grid.ys {
            for &x in &grid.xs {
                values.push(sample(field, x, y, sample_z)?);
            }
        }
        let segments = grid.segments(field, sample_z, &values)?;
        let loops = stitch_segments(&segments, config.stitch_tol_mm);
        let net_area_mm2: f64 = loops.iter().map(|contour| contour.signed_area_mm2).sum();
        let total_perimeter_mm: f64 = loops.iter().map(perimeter).sum();
        if !net_area_mm2.is_finite() || !total_perimeter_mm.is_finite() {
            return Err(SlicerError::InvalidConfig(
                "contour measurements are out of range",
            ));
        }
        layers.push(SliceLayer {
            layer_index,
            z_mm,
            loops,
            net_area_mm2,
            total_perimeter_mm,
        });
        bottom = z_mm;
        if bottom == max_z {
            break;
        }
    }
    Ok(SliceStack {
        config: *config,
        layers,
    })
}

fn validate_config(config: &SliceConfig) -> Result<(), SlicerError> {
    let lengths = [
        config.layer_height_mm,
        config.xy_step_mm,
        config.stitch_tol_mm,
    ];
    if lengths
        .into_iter()
        .any(|length| !length.is_finite() || length <= 0.0)
    {
        return Err(SlicerError::InvalidConfig(
            "layer height, XY step and stitch tolerance must be finite and positive",
        ));
    }
    for (min, max) in config.bbox_min_mm.into_iter().zip(config.bbox_max_mm) {
        if !min.is_finite() || !max.is_finite() || !(max - min).is_finite() || max <= min {
            return Err(SlicerError::InvalidConfig(
                "bounding-box extents must be finite and positive",
            ));
        }
    }
    Ok(())
}

fn cell_count(extent: f64, step: f64, limit: usize) -> Result<usize, SlicerError> {
    let ratio = extent / step;
    let nearest = ratio.round();
    // A whole number of steps can round a few ULPs above the integer.
    let count = if nearest >= 1.0 && (ratio - nearest).abs() <= 4.0 * f64::EPSILON * nearest {
        nearest
    } else {
        ratio.ceil().max(1.0)
    };
    if !count.is_finite() || count > limit as f64 {
        return Err(SlicerError::InvalidConfig(
            "grid or layer count is too large",
        ));
    }
    Ok(count as usize)
}

fn axis_coordinates(min: f64, max: f64, cells: usize) -> Result<Vec<f64>, SlicerError> {
    let mut coordinates = Vec::with_capacity(cells + 1);
    for index in 0..cells {
        coordinates.push((index as f64 / cells as f64).mul_add(max - min, min));
    }
    coordinates.push(max);
    if coordinates.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err(SlicerError::InvalidConfig(
            "XY step is below coordinate precision",
        ));
    }
    Ok(coordinates)
}

fn sample(
    field: &(impl SliceField + ?Sized),
    x_mm: f64,
    y_mm: f64,
    z_mm: f64,
) -> Result<f64, SlicerError> {
    let value = field.eval(x_mm, y_mm, z_mm);
    if !value.is_finite() {
        return Err(SlicerError::NonFiniteSample { x_mm, y_mm, z_mm });
    }
    Ok(value)
}

struct Grid {
    xs: Vec<f64>,
    ys: Vec<f64>,
}

#[derive(Clone, Copy)]
struct Crossing {
    edge: usize,
    point: [f64; 2],
}

struct Segment {
    start: Crossing,
    end: Crossing,
}

impl Grid {
    fn segments(
        &self,
        field: &(impl SliceField + ?Sized),
        z: f64,
        values: &[f64],
    ) -> Result<Vec<Segment>, SlicerError> {
        let columns = self.xs.len();
        let mut segments = Vec::new();
        for (row, ys) in self.ys.windows(2).enumerate() {
            for (column, xs) in self.xs.windows(2).enumerate() {
                let base = row * columns + column;
                // Corner order is counterclockwise from the bottom left.
                let nodes = [base, base + 1, base + columns + 1, base + columns];
                let corner_values = nodes.map(|node| values[node]);
                let inside = corner_values.map(|value| value < 0.0);
                if inside == [inside[0]; 4] {
                    continue;
                }

                let [x0, x1] = [xs[0], xs[1]];
                let [y0, y1] = [ys[0], ys[1]];
                let points = [[x0, y0], [x1, y0], [x1, y1], [x0, y1]];
                // Adjacent cells use the same ID and interpolation direction on
                // their common edge, even when a crossing hits a grid corner.
                let edge_nodes = [[0, 1], [1, 2], [3, 2], [0, 3]];
                let edge_ids = [
                    2 * nodes[0],
                    2 * nodes[1] + 1,
                    2 * nodes[3],
                    2 * nodes[0] + 1,
                ];
                let crossings = edge_nodes.map(|[a, b]| {
                    (inside[a] != inside[b]).then(|| {
                        interpolate(points[a], points[b], corner_values[a], corner_values[b])
                    })
                });
                let mut connect = |a: usize, b: usize| {
                    // An inside-to-outside edge in CCW traversal starts the
                    // directed segment, keeping material on its left.
                    let [start, end] = if inside[a] { [a, b] } else { [b, a] };
                    let crossing = |edge: usize| Crossing {
                        edge: edge_ids[edge],
                        point: crossings[edge].expect("selected edge crosses the contour"),
                    };
                    segments.push(Segment {
                        start: crossing(start),
                        end: crossing(end),
                    });
                };

                if crossings.iter().all(Option::is_some) {
                    // The center sign selects the noncrossing saddle pairing.
                    // Treat a center value of exactly zero as outside.
                    let center_inside = sample(field, x0.midpoint(x1), y0.midpoint(y1), z)? < 0.0;
                    if center_inside == inside[0] {
                        connect(0, 1);
                        connect(2, 3);
                    } else {
                        connect(3, 0);
                        connect(1, 2);
                    }
                } else {
                    let mut edges = (0..4).filter(|&index| crossings[index].is_some());
                    let a = edges.next().expect("mixed cell has two or four crossings");
                    let b = edges.next().expect("mixed cell has two or four crossings");
                    connect(a, b);
                }
            }
        }
        Ok(segments)
    }
}

fn interpolate(a: [f64; 2], b: [f64; 2], a_value: f64, b_value: f64) -> [f64; 2] {
    let a_mag = a_value.abs();
    let b_mag = b_value.abs();
    // Normalize before adding to avoid overflowing opposite-sign samples.
    let t = if a_mag <= b_mag {
        let ratio = a_mag / b_mag;
        ratio / (1.0 + ratio)
    } else {
        1.0 / (1.0 + b_mag / a_mag)
    };
    if t == 1.0 {
        b
    } else {
        [
            (b[0] - a[0]).mul_add(t, a[0]),
            (b[1] - a[1]).mul_add(t, a[1]),
        ]
    }
}

fn stitch_segments(segments: &[Segment], tolerance: f64) -> Vec<ContourLoop> {
    // Each crossing has at most one successor. Using grid edges prevents two
    // nearby contours from being welded to one another.
    let outgoing: HashMap<_, _> = segments
        .iter()
        .enumerate()
        .map(|(index, segment)| (segment.start.edge, index))
        .collect();
    let mut has_predecessor = vec![false; segments.len()];
    let successors: Vec<_> = segments
        .iter()
        .map(|segment| {
            let next = outgoing.get(&segment.end.edge).copied();
            if let Some(index) = next {
                has_predecessor[index] = true;
            }
            next
        })
        .collect();

    let mut loops = Vec::new();
    let mut visited = vec![false; segments.len()];
    let open_starts = (0..segments.len()).filter(|&index| !has_predecessor[index]);
    for start in open_starts.chain(0..segments.len()) {
        if visited[start] {
            continue;
        }
        let mut points = vec![segments[start].start.point];
        let mut index = start;
        let closed = loop {
            visited[index] = true;
            let next = successors[index];
            if next == Some(start) {
                break true;
            }
            let point = segments[index].end.point;
            if distance(points[points.len() - 1], point) > tolerance {
                points.push(point);
            }
            match next {
                Some(next) => index = next,
                None => break false,
            }
        };

        if closed && points.len() > 1 && distance(points[0], points[points.len() - 1]) <= tolerance
        {
            points.pop();
        }
        let area = if closed { signed_area(&points) } else { 0.0 };
        if (closed && points.len() >= 3 && area != 0.0) || (!closed && points.len() >= 2) {
            loops.push(ContourLoop {
                points_mm: points,
                signed_area_mm2: area,
                is_hole: area < 0.0,
                closed,
            });
        }
    }
    loops
}

fn signed_area(points: &[[f64; 2]]) -> f64 {
    let [x, y] = points[0];
    // Translating the shoelace sum avoids cancellation on offset workpieces.
    let twice_area: f64 = points
        .windows(2)
        .map(|pair| (pair[0][0] - x) * (pair[1][1] - y) - (pair[1][0] - x) * (pair[0][1] - y))
        .sum();
    twice_area * 0.5
}

fn perimeter(contour: &ContourLoop) -> f64 {
    let points = &contour.points_mm;
    let length: f64 = points
        .windows(2)
        .map(|pair| distance(pair[0], pair[1]))
        .sum();
    if contour.closed {
        length + distance(points[0], points[points.len() - 1])
    } else {
        length
    }
}

fn distance(a: [f64; 2], b: [f64; 2]) -> f64 {
    (a[0] - b[0]).hypot(a[1] - b[1])
}
