//! Common Layer Interface (CLI) v2.0 ASCII export and summary parsing.

use std::fmt::Write;

use crate::{ContourLoop, SliceLayer, SliceStack, SlicerError};

const VERSION: u32 = 200;

/// A compact description of a CLI ASCII contour stack.
#[derive(Debug, Clone, PartialEq)]
pub struct CliSummary {
    /// Millimeters represented by one geometry coordinate unit.
    pub units_mm: f64,
    /// The version header value (200 for v2.0).
    pub version: u32,
    pub layer_count: usize,
    pub polyline_count: usize,
    /// Polyline count for each layer, in file order.
    pub layer_polyline_counts: Vec<usize>,
}

/// Exports a contour stack as CLI v2.0 ASCII. Geometry coordinates are
/// expressed in units of `units_mm`, and the bounding box is in millimeters.
pub fn export_cli_ascii(stack: &SliceStack, units_mm: f64) -> Result<String, SlicerError> {
    let (units, scale) = decimal(units_mm)
        .filter(|(_, scale)| *scale > 0.0)
        .ok_or(SlicerError::InvalidUnits)?;

    let config = &stack.config;
    let mut dimensions = Vec::with_capacity(6);
    let mut bounds = [0.0; 6];
    for (index, value) in config
        .bbox_min_mm
        .into_iter()
        .chain(config.bbox_max_mm)
        .enumerate()
    {
        let (text, rounded) = decimal(value).ok_or(SlicerError::InvalidConfig(
            "CLI bounding box is out of range",
        ))?;
        dimensions.push(text);
        bounds[index] = rounded;
    }
    if !valid_bounds(bounds) {
        return Err(SlicerError::InvalidConfig(
            "CLI bounding box must have positive extents",
        ));
    }
    let layer_count = i32::try_from(stack.layers.len())
        .map_err(|_| SlicerError::InvalidConfig("too many CLI layers"))?;

    let mut output = format!(
        "$$HEADERSTART\n$$ASCII\n$$UNITS/{units}\n$$VERSION/{VERSION}\n\
         $$LABEL/1,\"Part\"\n$$DIMENSION/{}\n$$LAYERS/{layer_count}\n\
         $$HEADEREND\n$$GEOMETRYSTART\n",
        dimensions.join(","),
    );

    let mut previous_z = None;
    for layer in &stack.layers {
        let (z_text, z) = wire_coordinate(layer.z_mm, scale, layer)?;
        if previous_z.is_some_and(|previous| z <= previous) {
            return Err(contour_error(
                layer,
                "CLI layer heights must be strictly increasing",
            ));
        }
        previous_z = Some(z);
        writeln!(output, "$$LAYER/{z_text}").expect("writing to a String cannot fail");
        for contour in &layer.loops {
            write_polyline(&mut output, layer, contour, scale)?;
        }
    }
    output.push_str("$$GEOMETRYEND\n");
    Ok(output)
}

fn write_polyline(
    output: &mut String,
    layer: &SliceLayer,
    contour: &ContourLoop,
    scale: f64,
) -> Result<(), SlicerError> {
    if !contour.closed {
        return Err(contour_error(layer, "contour is open"));
    }
    let mut points = contour.points_mm.as_slice();
    if points.len() > 1 && points.first() == points.last() {
        points = &points[..points.len() - 1];
    }
    if points.len() < 3 {
        return Err(contour_error(
            layer,
            "contour has fewer than three vertices",
        ));
    }
    let count = i32::try_from(points.len() + 1)
        .map_err(|_| contour_error(layer, "contour has too many vertices"))?;
    let direction = u8::from(!contour.is_hole);
    // All contours describe the same part, identified by $$LABEL/1.
    write!(output, "$$POLYLINE/1,{direction},{count}").expect("writing to a String cannot fail");

    let mut first = [0.0; 2];
    let mut previous = [0.0; 2];
    let mut twice_area = 0.0;
    for (index, &[x, y]) in points.iter().chain(&points[..1]).enumerate() {
        let (x_text, x) = wire_coordinate(x, scale, layer)?;
        let (y_text, y) = wire_coordinate(y, scale, layer)?;
        let point = [x, y];
        if index == 0 {
            first = point;
        } else {
            if point == previous {
                return Err(contour_error(
                    layer,
                    "adjacent vertices coincide at CLI precision",
                ));
            }
            twice_area += (previous[0] - first[0]) * (y - first[1])
                - (x - first[0]) * (previous[1] - first[1]);
        }
        previous = point;
        write!(output, ",{x_text},{y_text}").expect("writing to a String cannot fail");
    }
    if twice_area == 0.0 || (twice_area < 0.0) != contour.is_hole {
        return Err(contour_error(
            layer,
            "CLI contour winding does not match its direction",
        ));
    }
    output.push('\n');
    Ok(())
}

fn wire_coordinate(
    value: f64,
    scale: f64,
    layer: &SliceLayer,
) -> Result<(String, f64), SlicerError> {
    decimal(value / scale).ok_or_else(|| contour_error(layer, "CLI coordinate is out of range"))
}

fn decimal(value: f64) -> Option<(String, f64)> {
    if !value.is_finite() || value.abs() >= 1e16 {
        return None;
    }
    // CLI reals use a decimal point, no exponent, and at most 16 digits.
    let whole_digits = value.abs().trunc().to_string().len();
    let precision = 16 - whole_digits;
    let text = format!("{value:.precision$}");
    let mut text = if precision == 0 {
        format!("{text}.")
    } else {
        text.trim_end_matches('0').to_owned()
    };
    if text.bytes().filter(u8::is_ascii_digit).count() > 16 {
        return None;
    }
    let rounded = text.parse::<f64>().ok()?;
    if rounded == 0.0 {
        text = "0.0".to_owned();
    }
    Some((text, rounded))
}

/// Validates the headers, layer ordering and closed polylines of an ASCII
/// contour stack and returns the observed counts.
pub fn parse_cli_ascii_summary(cli_text: &str) -> Result<CliSummary, SlicerError> {
    if !cli_text.is_ascii() {
        return Err(cli_error(0, "expected ASCII text"));
    }
    let mut records = cli_text.lines().enumerate().filter_map(|(index, line)| {
        let line = line
            .split_once("//")
            .map_or(line, |(record, _)| record)
            .trim();
        (!line.is_empty()).then_some((index + 1, line))
    });
    expect_record(records.next(), "$$HEADERSTART")?;

    let mut ascii = false;
    let mut units_mm = None;
    let mut version = None;
    let mut dimensions = false;
    let mut declared_layers = None;
    loop {
        let (line, record) = records
            .next()
            .ok_or_else(|| cli_error(0, "missing $$HEADEREND"))?;
        match record.split_once('/') {
            None if record == "$$ASCII" && !ascii => ascii = true,
            None if record == "$$HEADEREND" => break,
            Some(("$$UNITS", value)) if units_mm.is_none() => {
                let value = finite(value, line)?;
                if value <= 0.0 {
                    return Err(cli_error(line, "units must be positive"));
                }
                units_mm = Some(value);
            }
            Some(("$$VERSION", value)) if version.is_none() => {
                let parsed_version = value.trim().parse::<u32>().ok();
                if parsed_version != Some(VERSION) {
                    return Err(cli_error(line, "expected CLI version 200"));
                }
                version = parsed_version;
            }
            Some(("$$DIMENSION", value)) if !dimensions => {
                parse_dimensions(value, line)?;
                dimensions = true;
            }
            Some(("$$LAYERS", value)) if declared_layers.is_none() => {
                declared_layers = Some(nonnegative_int(value, line)?);
            }
            Some(("$$LABEL", value)) => {
                let (id, label) = value
                    .split_once(',')
                    .ok_or_else(|| cli_error(line, "expected a part label"))?;
                nonnegative_int(id, line)?;
                let label = label.trim();
                if label.len() < 2 || !label.starts_with('"') || !label.ends_with('"') {
                    return Err(cli_error(line, "expected a quoted part label"));
                }
            }
            Some(("$$DATE", value)) => {
                nonnegative_int(value, line)?;
            }
            _ => return Err(cli_error(line, "unexpected or duplicate CLI header record")),
        }
    }
    let units_mm = units_mm.ok_or_else(|| cli_error(0, "missing $$UNITS"))?;
    let version = version.ok_or_else(|| cli_error(0, "missing $$VERSION"))?;
    let layer_count = declared_layers.ok_or_else(|| cli_error(0, "missing $$LAYERS"))?;
    if !ascii || !dimensions {
        return Err(cli_error(0, "missing $$ASCII or $$DIMENSION"));
    }

    expect_record(records.next(), "$$GEOMETRYSTART")?;
    let mut counts = Vec::new();
    let mut previous_z = None;
    loop {
        let (line, record) = records
            .next()
            .ok_or_else(|| cli_error(0, "missing $$GEOMETRYEND"))?;
        if record == "$$GEOMETRYEND" {
            break;
        }
        match record.split_once('/') {
            Some(("$$LAYER", value)) => {
                let z = finite(value, line)?;
                if previous_z.is_some_and(|previous| z <= previous) {
                    return Err(cli_error(line, "layer heights must be strictly increasing"));
                }
                previous_z = Some(z);
                counts.push(0);
            }
            Some(("$$POLYLINE", value)) => {
                let count = counts
                    .last_mut()
                    .ok_or_else(|| cli_error(line, "polyline before first layer"))?;
                parse_polyline(value, line)?;
                *count += 1;
            }
            _ => return Err(cli_error(line, "unexpected CLI geometry record")),
        }
    }
    if let Some((line, _)) = records.next() {
        return Err(cli_error(line, "unexpected content after $$GEOMETRYEND"));
    }
    if counts.len() != layer_count {
        return Err(cli_error(0, "$$LAYERS count does not match the geometry"));
    }

    Ok(CliSummary {
        units_mm,
        version,
        layer_count,
        polyline_count: counts.iter().sum(),
        layer_polyline_counts: counts,
    })
}

/// Validates and parses an ASCII CLI contour stack back into layers, Z heights,
/// and closed contour polylines in millimetres for round-trip verification.
pub fn parse_cli_ascii(cli_text: &str) -> Result<SliceStack, SlicerError> {
    if !cli_text.is_ascii() {
        return Err(cli_error(0, "expected ASCII text"));
    }
    let mut records = cli_text.lines().enumerate().filter_map(|(index, line)| {
        let line = line
            .split_once("//")
            .map_or(line, |(record, _)| record)
            .trim();
        (!line.is_empty()).then_some((index + 1, line))
    });
    expect_record(records.next(), "$$HEADERSTART")?;

    let mut ascii = false;
    let mut units_mm = None;
    let mut version = None;
    let mut dimensions: Option<[f64; 6]> = None;
    let mut declared_layers = None;
    loop {
        let (line, record) = records
            .next()
            .ok_or_else(|| cli_error(0, "missing $$HEADEREND"))?;
        match record.split_once('/') {
            None if record == "$$ASCII" && !ascii => ascii = true,
            None if record == "$$HEADEREND" => break,
            Some(("$$UNITS", value)) if units_mm.is_none() => {
                let value = finite(value, line)?;
                if value <= 0.0 {
                    return Err(cli_error(line, "units must be positive"));
                }
                units_mm = Some(value);
            }
            Some(("$$VERSION", value)) if version.is_none() => {
                let parsed_version = value.trim().parse::<u32>().ok();
                if parsed_version != Some(VERSION) {
                    return Err(cli_error(line, "expected CLI version 200"));
                }
                version = parsed_version;
            }
            Some(("$$DIMENSION", value)) if dimensions.is_none() => {
                dimensions = Some(parse_dimensions_bounds(value, line)?);
            }
            Some(("$$LAYERS", value)) if declared_layers.is_none() => {
                declared_layers = Some(nonnegative_int(value, line)?);
            }
            Some(("$$LABEL", value)) => {
                let (id, label) = value
                    .split_once(',')
                    .ok_or_else(|| cli_error(line, "expected a part label"))?;
                nonnegative_int(id, line)?;
                let label = label.trim();
                if label.len() < 2 || !label.starts_with('"') || !label.ends_with('"') {
                    return Err(cli_error(line, "expected a quoted part label"));
                }
            }
            Some(("$$DATE", value)) => {
                nonnegative_int(value, line)?;
            }
            _ => return Err(cli_error(line, "unexpected or duplicate CLI header record")),
        }
    }
    let units_mm = units_mm.ok_or_else(|| cli_error(0, "missing $$UNITS"))?;
    let _version = version.ok_or_else(|| cli_error(0, "missing $$VERSION"))?;
    let layer_count = declared_layers.ok_or_else(|| cli_error(0, "missing $$LAYERS"))?;
    let bounds = dimensions.ok_or_else(|| cli_error(0, "missing $$ASCII or $$DIMENSION"))?;
    if !ascii {
        return Err(cli_error(0, "missing $$ASCII or $$DIMENSION"));
    }

    expect_record(records.next(), "$$GEOMETRYSTART")?;
    let mut layers: Vec<SliceLayer> = Vec::with_capacity(layer_count);
    let mut previous_z = None;
    loop {
        let (line, record) = records
            .next()
            .ok_or_else(|| cli_error(0, "missing $$GEOMETRYEND"))?;
        if record == "$$GEOMETRYEND" {
            break;
        }
        match record.split_once('/') {
            Some(("$$LAYER", value)) => {
                let z = finite(value, line)? * units_mm;
                if previous_z.is_some_and(|previous| z <= previous) {
                    return Err(cli_error(line, "layer heights must be strictly increasing"));
                }
                previous_z = Some(z);
                layers.push(SliceLayer {
                    layer_index: layers.len(),
                    z_mm: z,
                    loops: Vec::new(),
                    net_area_mm2: 0.0,
                    total_perimeter_mm: 0.0,
                });
            }
            Some(("$$POLYLINE", value)) => {
                let layer = layers
                    .last_mut()
                    .ok_or_else(|| cli_error(line, "polyline before first layer"))?;
                let contour = parse_polyline_loop(value, line, units_mm)?;
                layer.net_area_mm2 += contour.signed_area_mm2;
                layer.total_perimeter_mm += loop_perimeter(&contour);
                layer.loops.push(contour);
            }
            _ => return Err(cli_error(line, "unexpected CLI geometry record")),
        }
    }
    if let Some((line, _)) = records.next() {
        return Err(cli_error(line, "unexpected content after $$GEOMETRYEND"));
    }
    if layers.len() != layer_count {
        return Err(cli_error(0, "$$LAYERS count does not match the geometry"));
    }

    let inferred_layer_height = if layers.len() >= 2 {
        layers[1].z_mm - layers[0].z_mm
    } else {
        (bounds[5] - bounds[2]).max(1e-6)
    };
    let config = crate::SliceConfig {
        bbox_min_mm: [bounds[0], bounds[1], bounds[2]],
        bbox_max_mm: [bounds[3], bounds[4], bounds[5]],
        layer_height_mm: inferred_layer_height,
        xy_step_mm: 1.0,
        stitch_tol_mm: 1e-6,
    };

    Ok(SliceStack { config, layers })
}

fn parse_dimensions(value: &str, line: usize) -> Result<(), SlicerError> {
    parse_dimensions_bounds(value, line)?;
    Ok(())
}

fn parse_dimensions_bounds(value: &str, line: usize) -> Result<[f64; 6], SlicerError> {
    let mut fields = value.split(',');
    let mut bounds = [0.0; 6];
    for coordinate in &mut bounds {
        *coordinate = read_coordinate(&mut fields, line)?;
    }
    if fields.next().is_some() || !valid_bounds(bounds) {
        return Err(cli_error(
            line,
            "expected a bounding box with positive extents",
        ));
    }
    Ok(bounds)
}

fn parse_polyline_loop(
    value: &str,
    line: usize,
    units_mm: f64,
) -> Result<ContourLoop, SlicerError> {
    let mut fields = value.split(',');
    let mut read_integer = || {
        nonnegative_int(
            fields
                .next()
                .ok_or_else(|| cli_error(line, "missing polyline parameter"))?,
            line,
        )
    };
    read_integer()?; // Part identifier.
    let direction = read_integer()?;
    let count = read_integer()?;
    if direction > 1 || count < 4 {
        return Err(cli_error(
            line,
            "expected a closed polyline with at least three vertices",
        ));
    }

    let mut read_point = || {
        Ok([
            read_coordinate(&mut fields, line)? * units_mm,
            read_coordinate(&mut fields, line)? * units_mm,
        ])
    };
    let mut points = Vec::with_capacity(count);
    for _ in 0..count {
        points.push(read_point()?);
    }
    if fields.next().is_some() {
        return Err(cli_error(
            line,
            "polyline point count does not match its coordinates",
        ));
    }
    if points.first() != points.last() {
        return Err(cli_error(line, "polyline is not explicitly closed"));
    }
    points.pop();
    let area = loop_signed_area(&points);
    Ok(ContourLoop {
        points_mm: points,
        signed_area_mm2: area,
        is_hole: direction == 0,
        closed: true,
    })
}

fn loop_signed_area(points: &[[f64; 2]]) -> f64 {
    let [x, y] = points[0];
    let twice_area: f64 = points
        .windows(2)
        .map(|pair| (pair[0][0] - x) * (pair[1][1] - y) - (pair[1][0] - x) * (pair[0][1] - y))
        .sum();
    twice_area * 0.5
}

fn loop_perimeter(contour: &ContourLoop) -> f64 {
    let pts = &contour.points_mm;
    let length: f64 = pts
        .windows(2)
        .map(|pair| (pair[0][0] - pair[1][0]).hypot(pair[0][1] - pair[1][1]))
        .sum();
    if contour.closed && pts.len() >= 2 {
        length + (pts[0][0] - pts[pts.len() - 1][0]).hypot(pts[0][1] - pts[pts.len() - 1][1])
    } else {
        length
    }
}

fn parse_polyline(value: &str, line: usize) -> Result<(), SlicerError> {
    let mut fields = value.split(',');
    let mut read_integer = || {
        nonnegative_int(
            fields
                .next()
                .ok_or_else(|| cli_error(line, "missing polyline parameter"))?,
            line,
        )
    };
    read_integer()?; // Part identifier.
    let direction = read_integer()?;
    let count = read_integer()?;
    if direction > 1 || count < 4 {
        return Err(cli_error(
            line,
            "expected a closed polyline with at least three vertices",
        ));
    }

    let mut read_point = || {
        Ok([
            read_coordinate(&mut fields, line)?,
            read_coordinate(&mut fields, line)?,
        ])
    };
    let first = read_point()?;
    let mut last = first;
    for _ in 1..count {
        last = read_point()?;
    }
    if fields.next().is_some() {
        return Err(cli_error(
            line,
            "polyline point count does not match its coordinates",
        ));
    }
    if first != last {
        return Err(cli_error(line, "polyline is not explicitly closed"));
    }
    Ok(())
}

fn valid_bounds(bounds: [f64; 6]) -> bool {
    (0..3).all(|axis| bounds[axis] < bounds[axis + 3])
}

fn read_coordinate<'a>(
    fields: &mut impl Iterator<Item = &'a str>,
    line: usize,
) -> Result<f64, SlicerError> {
    finite(
        fields
            .next()
            .ok_or_else(|| cli_error(line, "missing coordinate"))?,
        line,
    )
}

fn finite(value: &str, line: usize) -> Result<f64, SlicerError> {
    value
        .trim()
        .parse::<f64>()
        .ok()
        .filter(|value| value.is_finite())
        .ok_or_else(|| cli_error(line, "expected a finite number"))
}

fn nonnegative_int(value: &str, line: usize) -> Result<usize, SlicerError> {
    value
        .trim()
        .parse::<i32>()
        .ok()
        .and_then(|value| usize::try_from(value).ok())
        .ok_or_else(|| cli_error(line, "expected a nonnegative CLI integer"))
}

fn expect_record(record: Option<(usize, &str)>, expected: &str) -> Result<(), SlicerError> {
    match record {
        Some((_, line)) if line == expected => Ok(()),
        _ => Err(cli_error(
            record.map_or(0, |(line, _)| line),
            format!("expected {expected}"),
        )),
    }
}

fn contour_error(layer: &SliceLayer, reason: &'static str) -> SlicerError {
    SlicerError::InvalidContour {
        layer_index: layer.layer_index,
        reason,
    }
}

fn cli_error(line: usize, reason: impl Into<String>) -> SlicerError {
    SlicerError::InvalidCli {
        line,
        reason: reason.into(),
    }
}
