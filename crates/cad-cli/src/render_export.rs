//! 把编译好的实体画成离屏 PNG。没打开 `wgpu-renderer` 时只报告原因，不链接 GPU。

use cad_kernel_occt::{BBox3, SolidShape};
use cad_render::ViewPreset;
#[cfg(feature = "wgpu-renderer")]
use std::fs;
use std::path::Path;
#[cfg(feature = "wgpu-renderer")]
use std::path::PathBuf;

pub struct RenderPaths {
    pub output: String,
    pub semantic: Option<String>,
}

pub fn export(
    solid: &SolidShape,
    bbox: &BBox3,
    output: &Path,
    views_text: &str,
    size_text: &str,
    semantic: bool,
) -> Result<RenderPaths, String> {
    let views = parse_views(views_text)?;
    let (width, height) = parse_size(size_text)?;
    #[cfg(not(feature = "wgpu-renderer"))]
    {
        let _ = (solid, bbox, output, semantic, width, height, views);
        Err(
            "this cad-cli was built without --features wgpu-renderer, so shaded images cannot be written"
                .into(),
        )
    }
    #[cfg(feature = "wgpu-renderer")]
    export_with_gpu(solid, bbox, output, &views, width, height, semantic)
}

fn parse_views(text: &str) -> Result<Vec<(String, ViewPreset)>, String> {
    let mut views = Vec::new();
    for raw in text.split(',') {
        let name = raw.trim().to_ascii_lowercase();
        if name.is_empty() {
            return Err("render view name is empty".into());
        }
        let preset = match name.as_str() {
            "iso" => ViewPreset::Iso,
            "front" => ViewPreset::Front,
            "top" => ViewPreset::Top,
            "right" => ViewPreset::Right,
            other => {
                return Err(format!(
                    "unknown render view `{other}`; use iso, front, top, right"
                ));
            }
        };
        if views.iter().any(|(seen, _)| seen == &name) {
            return Err(format!("render view `{name}` is listed twice"));
        }
        views.push((name, preset));
    }
    if views.is_empty() {
        return Err("choose at least one render view".into());
    }
    Ok(views)
}

fn parse_size(text: &str) -> Result<(u32, u32), String> {
    let trimmed = text.trim();
    let Some((width_text, height_text)) =
        trimmed.split_once('x').or_else(|| trimmed.split_once('X'))
    else {
        return Err(format!("render size `{text}` must look like 1024x768"));
    };
    let width: u32 = width_text
        .trim()
        .parse()
        .map_err(|_| format!("bad render width `{width_text}`"))?;
    let height: u32 = height_text
        .trim()
        .parse()
        .map_err(|_| format!("bad render height `{height_text}`"))?;
    if width == 0 || height == 0 || width > 4096 || height > 4096 {
        return Err("render size must be between 1 and 4096 pixels".into());
    }
    Ok((width, height))
}

#[cfg(feature = "wgpu-renderer")]
fn is_png(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("png"))
}

#[cfg(feature = "wgpu-renderer")]
struct Shot {
    name: String,
    image: PathBuf,
    mask: PathBuf,
}

#[cfg(feature = "wgpu-renderer")]
fn shots(output: &Path, views: &[(String, ViewPreset)]) -> Result<(Vec<Shot>, PathBuf), String> {
    if is_png(output) {
        if views.len() != 1 {
            return Err(
                "pass a directory to --out-render when exporting more than one view".into(),
            );
        }
        let stem = output
            .file_stem()
            .and_then(|stem| stem.to_str())
            .ok_or_else(|| format!("render path `{}` is not valid UTF-8", output.display()))?;
        let mask = output.with_file_name(format!("{stem}.mask.png"));
        let legend = output.with_file_name(format!("{stem}.semantic.json"));
        return Ok((
            vec![Shot {
                name: views[0].0.clone(),
                image: output.to_path_buf(),
                mask,
            }],
            legend,
        ));
    }
    Ok((
        views
            .iter()
            .map(|(name, _)| Shot {
                name: name.clone(),
                image: output.join(format!("{name}.png")),
                mask: output.join(format!("{name}.mask.png")),
            })
            .collect(),
        output.join("semantic.json"),
    ))
}

#[cfg(feature = "wgpu-renderer")]
fn write_bytes(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)
            .map_err(|err| format!("create `{}`: {err}", parent.display()))?;
    }
    fs::write(path, bytes).map_err(|err| format!("write `{}`: {err}", path.display()))
}

#[cfg(feature = "wgpu-renderer")]
fn export_with_gpu(
    solid: &SolidShape,
    bbox: &BBox3,
    output: &Path,
    views: &[(String, ViewPreset)],
    width: u32,
    height: u32,
    semantic: bool,
) -> Result<RenderPaths, String> {
    use cad_render::{CompactScene, RenderEngine, RenderOptions};
    use serde::Serialize;

    let (shots, legend_path) = shots(output, views)?;
    if !is_png(output) {
        fs::create_dir_all(output)
            .map_err(|err| format!("create `{}`: {err}", output.display()))?;
    }
    let scene = CompactScene::from_solid(solid);
    let engine = RenderEngine::new(width, height)?;
    let mut legends = Vec::new();
    for (shot, (_, preset)) in shots.iter().zip(views) {
        let camera = framed_camera(*preset, bbox);
        let frame = engine.render(&scene, &camera, &RenderOptions::default())?;
        write_bytes(&shot.image, &frame.image.to_png()?)?;
        if semantic {
            let (mask, entries) = frame.semantic_mask(&scene);
            write_bytes(&shot.mask, &mask.to_png()?)?;
            legends.push(ViewLegend {
                view: shot.name.clone(),
                image: shot.image.to_string_lossy().into_owned(),
                mask: shot.mask.to_string_lossy().into_owned(),
                entries,
            });
        }
    }
    let semantic_path = if semantic {
        #[derive(Serialize)]
        struct SemanticDocument {
            width: u32,
            height: u32,
            views: Vec<ViewLegend>,
        }
        let json = serde_json::to_string_pretty(&SemanticDocument {
            width,
            height,
            views: legends,
        })
        .map_err(|err| format!("semantic legend: {err}"))?;
        write_bytes(&legend_path, json.as_bytes())?;
        Some(legend_path.to_string_lossy().into_owned())
    } else {
        None
    };
    Ok(RenderPaths {
        output: output.to_string_lossy().into_owned(),
        semantic: semantic_path,
    })
}

#[cfg(feature = "wgpu-renderer")]
fn framed_camera(preset: ViewPreset, bbox: &BBox3) -> cad_render::Camera {
    let (center, span) = if bbox.is_valid() {
        let size = bbox.size();
        let diagonal = (size[0] * size[0] + size[1] * size[1] + size[2] * size[2])
            .sqrt()
            .max(1.0);
        (
            [
                (bbox.min[0] + bbox.max[0]) * 0.5,
                (bbox.min[1] + bbox.max[1]) * 0.5,
                (bbox.min[2] + bbox.max[2]) * 0.5,
            ],
            diagonal,
        )
    } else {
        ([0.0; 3], 100.0)
    };
    let mut camera = cad_render::Camera::preset(preset, center, span * 4.0);
    camera.ortho_height_mm = span * 1.35;
    camera
}

#[cfg(feature = "wgpu-renderer")]
#[derive(serde::Serialize)]
struct ViewLegend {
    view: String,
    image: String,
    mask: String,
    entries: Vec<cad_render::SemanticEntry>,
}
