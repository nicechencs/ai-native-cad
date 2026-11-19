use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize)]
pub struct EngineStatus {
    pub runtime: &'static str,
    pub engine_version: &'static str,
    pub occt_available: bool,
    pub wgpu_backend: &'static str,
    pub offline_ready: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CompileKclRequest {
    pub file_name: String,
    pub source: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ExportRequest {
    pub file_name: String,
    pub source: String,
    pub out_path: String,
    pub include_pmi: Option<bool>,
    pub views: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExportResult {
    pub format: &'static str,
    pub out_path: String,
    pub bytes_written: u64,
    pub pmi_entity_count: usize,
    pub message: String,
}

pub mod commands {
    use super::{CompileKclRequest, EngineStatus, ExportRequest, ExportResult};

    #[tauri::command]
    pub fn get_engine_status() -> EngineStatus {
        EngineStatus {
            runtime: "tauri-v2",
            engine_version: env!("CARGO_PKG_VERSION"),
            occt_available: cfg!(feature = "occt-ffi"),
            wgpu_backend: "wgpu-native",
            offline_ready: true,
        }
    }

    #[tauri::command]
    pub fn compile_kcl(request: CompileKclRequest) -> Result<serde_json::Value, String> {
        if request.source.trim().is_empty() {
            return Err("KCL source cannot be empty".to_string());
        }
        Ok(serde_json::json!({
            "status": "ok",
            "file": request.file_name,
            "diagnostics": [],
            "metrics": {
                "watertight": true,
                "volume_mm3": 0.0,
                "bbox_mm": { "min": [0.0, 0.0, 0.0], "max": [0.0, 0.0, 0.0] },
                "face_count": 0,
                "edge_count": 0
            }
        }))
    }

    #[tauri::command]
    pub fn build_cad_artifact(request: CompileKclRequest) -> Result<serde_json::Value, String> {
        let diagnostics = compile_kcl(request.clone())?;
        Ok(serde_json::json!({
            "diagnostics": diagnostics,
            "scene_mesh": {
                "format_version": "0.1",
                "part_name": request.file_name,
                "units": "mm",
                "faces": [],
                "edges": [],
                "edit_handles": []
            },
            "elapsed_ms": 12
        }))
    }

    #[tauri::command]
    pub fn export_step_ap242(request: ExportRequest) -> Result<ExportResult, String> {
        Ok(ExportResult {
            format: "STEP_AP242",
            out_path: request.out_path,
            bytes_written: request.source.len() as u64,
            pmi_entity_count: usize::from(request.include_pmi.unwrap_or(true)),
            message: format!("Prepared STEP AP242 export for {}", request.file_name),
        })
    }

    #[tauri::command]
    pub fn export_dxf_drawing(request: ExportRequest) -> Result<ExportResult, String> {
        let view_count = request.views.as_ref().map_or(3, Vec::len);
        Ok(ExportResult {
            format: "DXF",
            out_path: request.out_path,
            bytes_written: 0,
            pmi_entity_count: 0,
            message: format!(
                "Prepared {} orthographic DXF views for {}",
                view_count, request.file_name
            ),
        })
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            commands::get_engine_status,
            commands::compile_kcl,
            commands::build_cad_artifact,
            commands::export_step_ap242,
            commands::export_dxf_drawing,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
