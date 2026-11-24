//! Nominal room-temperature properties for common engineering alloys and polymers.
//!
//! Loaded exclusively from `schemas/materials.json`, the single source of truth
//! shared with `cad-render`, `studio-ui`, and `cad-agent`. Values are handbook
//! starting points for mass, stiffness, and thermal estimates. They are not a
//! certified material certificate.

use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use crate::StdLibError;

/// Physical properties of one catalog material.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MaterialSpec {
    /// Stable canonical id, for example `Al-6061-T6` or `SUS304`.
    pub id: String,
    /// Short display name.
    pub name: String,
    /// Mass density, kilograms per cubic metre.
    pub density_kg_m3: f64,
    /// Young's modulus, gigapascals.
    pub youngs_modulus_gpa: f64,
    /// Poisson's ratio, dimensionless.
    pub poissons_ratio: f64,
    /// Yield strength, megapascals.
    pub yield_strength_mpa: f64,
    /// Ultimate tensile strength, megapascals.
    pub ultimate_strength_mpa: f64,
    /// Thermal conductivity, watts per metre-kelvin.
    pub thermal_conductivity_w_mk: f64,
    /// Linear thermal expansion, micrometres per metre-kelvin.
    pub cte_um_mk: f64,
}

#[derive(Deserialize)]
struct MaterialCatalog {
    materials: Vec<MaterialCatalogEntry>,
}

#[derive(Deserialize)]
struct MaterialCatalogEntry {
    id: String,
    #[serde(default)]
    aliases: Vec<String>,
    name: String,
    #[serde(default)]
    english_name: Option<String>,
    density_g_cm3: f64,
    youngs_modulus_gpa: f64,
    poissons_ratio: f64,
    yield_strength_mpa: f64,
    ultimate_strength_mpa: f64,
    thermal_conductivity_w_mk: f64,
    cte_um_mk: f64,
}

fn material_catalog() -> &'static [MaterialCatalogEntry] {
    static CATALOG: OnceLock<Vec<MaterialCatalogEntry>> = OnceLock::new();
    CATALOG.get_or_init(|| {
        let parsed: MaterialCatalog =
            serde_json::from_str(include_str!("../../../schemas/materials.json"))
                .expect("schemas/materials.json must be valid JSON");
        parsed.materials
    })
}

fn spec_from(row: &MaterialCatalogEntry) -> MaterialSpec {
    MaterialSpec {
        id: row.id.clone(),
        name: row
            .english_name
            .clone()
            .unwrap_or_else(|| row.name.clone()),
        density_kg_m3: row.density_g_cm3 * 1000.0,
        youngs_modulus_gpa: row.youngs_modulus_gpa,
        poissons_ratio: row.poissons_ratio,
        yield_strength_mpa: row.yield_strength_mpa,
        ultimate_strength_mpa: row.ultimate_strength_mpa,
        thermal_conductivity_w_mk: row.thermal_conductivity_w_mk,
        cte_um_mk: row.cte_um_mk,
    }
}

/// Look up a material by its canonical id or any registered alias in `schemas/materials.json`.
pub fn lookup_material(id: &str) -> Result<MaterialSpec, StdLibError> {
    let key = id.trim();
    if key.is_empty() {
        return Err(StdLibError::UnknownMaterial(key.to_string()));
    }
    material_catalog()
        .iter()
        .find(|row| {
            row.id.eq_ignore_ascii_case(key)
                || row.aliases.iter().any(|alias| alias.eq_ignore_ascii_case(key))
        })
        .map(spec_from)
        .ok_or_else(|| StdLibError::UnknownMaterial(key.to_string()))
}

/// Mass in kilograms of `volume_mm3` of `material`.
///
/// `1 mm³ = 1e-9 m³`, so mass = density × volume × 1e-9.
pub fn mass_kg(volume_mm3: f64, material: &MaterialSpec) -> f64 {
    material.density_kg_m3 * volume_mm3 * 1.0e-9
}
