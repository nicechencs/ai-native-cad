"""Write and inspect ``.aicad`` containers (single-graph dir, sharded dir, ZIP, and SQLite)."""

from __future__ import annotations

import json
import os
import re
import sqlite3
import stat
import unicodedata
import zipfile
from collections.abc import Mapping
from pathlib import Path
from types import MappingProxyType
from typing import Any

from aicad._schema_constants import CONTAINER_FORMAT_VERSION, IR_SCHEMA_VERSION
from aicad.curves import sha256_hex
from aicad.graph import GraphBuilder, canonical_json

DEFAULT_KERNEL_FINGERPRINT: MappingProxyType[str, str] = MappingProxyType(
    {"kernel_id": "aicad-python-sdk", "kernel_version": "0.1.0"}
)
_KERNEL = dict(DEFAULT_KERNEL_FINGERPRINT)
_SHARDED_ENTRY_GRAPH = "graphs/00_header.ir.json"
_DEFAULT_GRAPH_SHARD = "default"
_KCL_POLICIES = {"on_demand_dynamic", "materialized_synced"}
_SQLITE_MAGIC = b"SQLite format 3\x00"
_SQLITE_APP_ID = 0x41494344
_RESERVED_DOS_NAMES = re.compile(r"^(?:CON|PRN|AUX|NUL|COM[1-9]|LPT[1-9])$", re.IGNORECASE)


def save_aicad_directory(
    dest_dir: str | Path,
    project_name: str,
    builder: GraphBuilder,
    *,
    kernel_fingerprint: Mapping[str, str] | None = None,
) -> Path:
    """Write ``manifest.json``, ``graph.ir.json``, ``engineering.json``, blobs, and drawings.

    ``kcl_view`` is null, so the directory does not need ``views/main.kcl``.
    Blob bytes are checked against the SHA-256 and length stored in the graph
    before they are written. Each registered drawing sheet is written to
    ``drawings/<sheet_id>.drawing.json``.
    """
    _validate_save_inputs(project_name, builder)
    resolved_kernel = _resolve_kernel_fingerprint(kernel_fingerprint)
    dest = Path(dest_dir)
    _reject_symlink_or_reparse(dest)

    graph = builder.to_graph_dict()
    manifest = {
        "format_version": CONTAINER_FORMAT_VERSION,
        "ir_schema_version": IR_SCHEMA_VERSION,
        "project_name": project_name,
        "created_with_kernel": resolved_kernel,
        "entry_graph": "graph.ir.json",
        "kcl_view": None,
    }
    engineering = {"appearances": [], "bom": [], "pmi": []}
    payloads = _verified_blob_payloads(graph, builder)
    sheets = _verified_drawing_sheets(builder)

    dest.mkdir(parents=True, exist_ok=True)
    _remove_stale_shards(dest, keep_files=set())
    graphs_dir = dest / "graphs"
    if graphs_dir.is_dir() and not any(graphs_dir.iterdir()):
        graphs_dir.rmdir()

    _write_text(dest / "manifest.json", _pretty(manifest))
    _write_text(dest / "engineering.json", _pretty(engineering))
    _write_text(dest / "graph.ir.json", canonical_json(graph) + "\n")
    for relative, payload in payloads.items():
        _write_blob(dest, relative, payload)
    for sheet_id, sheet in sheets.items():
        drawing = dest / "drawings" / f"{sheet_id}.drawing.json"
        drawing.parent.mkdir(parents=True, exist_ok=True)
        _write_text(drawing, _pretty(sheet))
    return dest


def save_aicad_sharded_directory(
    dest_dir: str | Path,
    project_name: str,
    builder: GraphBuilder,
    *,
    kcl_projection_policy: str = "on_demand_dynamic",
    kernel_fingerprint: Mapping[str, str] | None = None,
) -> Path:
    """Write a Git-friendly sharded ``.aicad`` directory compatible with ``cad-container``.

    Writes ``manifest.json`` pointing at ``graphs/00_header.ir.json`` plus one
    ``graphs/<shard>.ir.json`` per module (grouped by ``node["metadata"]["module"]``,
    defaulting to ``default``).
    """
    _validate_save_inputs(project_name, builder)
    resolved_kernel = _resolve_kernel_fingerprint(kernel_fingerprint)
    if kcl_projection_policy not in _KCL_POLICIES:
        raise ValueError(
            "kcl_projection_policy must be 'on_demand_dynamic' or 'materialized_synced', "
            f"got {kcl_projection_policy!r}"
        )

    dest = Path(dest_dir)
    _reject_symlink_or_reparse(dest)

    graph = builder.to_graph_dict()
    payloads = _verified_blob_payloads(graph, builder)
    sheets = _verified_drawing_sheets(builder)

    header: dict[str, Any] = {
        "ir_schema_version": graph["ir_schema_version"],
        "graph_id": graph["graph_id"],
        "units": graph["units"],
        "parameters": graph["parameters"],
        "blobs": graph["blobs"],
    }
    if graph.get("external_refs"):
        header["external_refs"] = graph["external_refs"]
    header["outputs"] = graph["outputs"]

    buckets: dict[str, dict[str, Any]] = {}
    for node_id in sorted(graph["nodes"]):
        node = graph["nodes"][node_id]
        metadata = node.get("metadata")
        raw_module = ""
        if isinstance(metadata, Mapping):
            candidate = metadata.get("module")
            if isinstance(candidate, str):
                raw_module = candidate
        shard_name = _checked_shard_name(raw_module if raw_module else _DEFAULT_GRAPH_SHARD)
        buckets.setdefault(shard_name, {})[node_id] = node

    manifest: dict[str, Any] = {
        "format_version": CONTAINER_FORMAT_VERSION,
        "ir_schema_version": IR_SCHEMA_VERSION,
        "project_name": project_name,
        "created_with_kernel": resolved_kernel,
        "entry_graph": _SHARDED_ENTRY_GRAPH,
        "kcl_view": None,
    }
    if kcl_projection_policy != "materialized_synced":
        manifest["kcl_projection_policy"] = kcl_projection_policy

    engineering = {"appearances": [], "bom": [], "pmi": []}

    dest.mkdir(parents=True, exist_ok=True)
    single_graph_file = dest / "graph.ir.json"
    if single_graph_file.is_file():
        single_graph_file.unlink()
    if kcl_projection_policy == "on_demand_dynamic":
        kcl_file = dest / "views" / "main.kcl"
        if kcl_file.is_file():
            kcl_file.unlink()

    graphs_dir = dest / "graphs"
    graphs_dir.mkdir(parents=True, exist_ok=True)

    written_shard_names = {"00_header.ir.json"}
    _write_text(graphs_dir / "00_header.ir.json", _pretty(header))
    for shard_name in sorted(buckets):
        filename = f"{shard_name}.ir.json"
        written_shard_names.add(filename)
        shard_doc = {
            "shard": shard_name,
            "nodes": buckets[shard_name],
        }
        _write_text(graphs_dir / filename, _pretty(shard_doc))

    _remove_stale_shards(dest, keep_files=written_shard_names)
    _write_text(dest / "manifest.json", _pretty(manifest))
    _write_text(dest / "engineering.json", _pretty(engineering))

    for relative, payload in payloads.items():
        _write_blob(dest, relative, payload)
    for sheet_id, sheet in sheets.items():
        drawing = dest / "drawings" / f"{sheet_id}.drawing.json"
        drawing.parent.mkdir(parents=True, exist_ok=True)
        _write_text(drawing, _pretty(sheet))
    return dest


def inspect_aicad_container_metadata(path: str | Path) -> dict[str, Any]:
    """Inspect metadata and entity counts of any ``.aicad`` container without loading binary blobs.

    Supports ``directory`` (single-graph), ``sharded_directory``, ``archive`` (ZIP),
    and ``sqlite`` (``SQLite format 3\\0`` with ``application_id == 0x41494344``).
    """
    target = Path(path)
    _reject_symlink_or_reparse(target, must_exist=True)

    if target.is_dir():
        return _inspect_directory(target)
    if target.is_file():
        with target.open("rb") as handle:
            magic = handle.read(16)
        if magic == _SQLITE_MAGIC:
            return _inspect_sqlite(target)
        if len(magic) >= 4 and magic[:4] in (b"PK\x03\x04", b"PK\x05\x06", b"PK\x07\x08"):
            return _inspect_zip(target)
        raise ValueError(f"unsupported or unrecognized container format at {target}")
    raise ValueError(f"unsafe container path {target}")


def _validate_container_versions(
    format_version: Any,
    ir_schema_version: Any,
    source: Path | str,
) -> tuple[int, int]:
    if isinstance(format_version, bool) or not isinstance(format_version, int) or format_version < 1:
        raise ValueError(f"invalid container format_version {format_version!r} in {source}")
    if (
        isinstance(ir_schema_version, bool)
        or not isinstance(ir_schema_version, int)
        or ir_schema_version < 1
    ):
        raise ValueError(f"invalid ir_schema_version {ir_schema_version!r} in {source}")
    if format_version > CONTAINER_FORMAT_VERSION:
        raise ValueError(
            f"unsupported container format_version {format_version} "
            f"(maximum supported is {CONTAINER_FORMAT_VERSION}) in {source}"
        )
    if ir_schema_version > IR_SCHEMA_VERSION:
        raise ValueError(
            f"unsupported ir_schema_version {ir_schema_version} "
            f"(maximum supported is {IR_SCHEMA_VERSION}) in {source}"
        )
    return format_version, ir_schema_version


def _validate_ir_version_if_present(doc: Mapping[str, Any], source: Path | str) -> None:
    if "ir_schema_version" not in doc:
        return
    raw = doc["ir_schema_version"]
    if isinstance(raw, bool) or not isinstance(raw, int) or raw < 1:
        raise ValueError(f"invalid ir_schema_version {raw!r} in {source}")
    if raw > IR_SCHEMA_VERSION:
        raise ValueError(
            f"unsupported ir_schema_version {raw} "
            f"(maximum supported is {IR_SCHEMA_VERSION}) in {source}"
        )


def _inspect_directory(dir_path: Path) -> dict[str, Any]:
    manifest_path = dir_path / "manifest.json"
    _reject_symlink_or_reparse(manifest_path, must_exist=True)
    manifest = _load_json_dict(manifest_path)
    format_version, ir_schema_version = _validate_container_versions(
        manifest.get("format_version", CONTAINER_FORMAT_VERSION),
        manifest.get("ir_schema_version", IR_SCHEMA_VERSION),
        manifest_path,
    )

    graphs_dir = dir_path / "graphs"
    shard_files: list[Path] = []
    if graphs_dir.exists():
        _reject_symlink_or_reparse(graphs_dir, must_exist=True)
        if not graphs_dir.is_dir():
            raise ValueError(f"unsafe graphs entry at {graphs_dir}")
        for child in sorted(graphs_dir.iterdir(), key=lambda p: p.name):
            _reject_symlink_or_reparse(child, must_exist=True)
            if child.is_file() and child.name.endswith(".ir.json"):
                shard_files.append(child)

    if shard_files:
        storage = "sharded_directory"
        header_path = graphs_dir / "00_header.ir.json"
        if not header_path.is_file():
            raise ValueError(f"missing sharded graph header at {header_path}")
        header = _load_json_dict(header_path)
        _validate_ir_version_if_present(header, header_path)
        merged_nodes: dict[str, Any] = {}
        shard_names: list[str] = []
        for shard_file in shard_files:
            if shard_file.name.lower() == "00_header.ir.json":
                continue
            shard_doc = _load_json_dict(shard_file)
            shard_label = shard_doc.get("shard")
            if isinstance(shard_label, str):
                shard_names.append(shard_label)
            nodes = shard_doc.get("nodes")
            if not isinstance(nodes, dict):
                raise ValueError(f"shard {shard_file.name} is missing a nodes object")
            for key, node in nodes.items():
                if not isinstance(node, dict) or node.get("id") != key:
                    raise ValueError(f"node id mismatch for {key!r} in {shard_file.name}")
                if key in merged_nodes:
                    raise ValueError(f"duplicate node id {key!r} across shards")
                merged_nodes[key] = node
        graph_id = str(header.get("graph_id", ""))
        node_count = len(merged_nodes)
        parameter_count = len(_expect_dict(header.get("parameters", {}), "parameters"))
        blob_count = len(_expect_dict(header.get("blobs", {}), "blobs"))
        external_ref_count = len(_expect_dict(header.get("external_refs", {}), "external_refs"))
    else:
        storage = "directory"
        shard_names = []
        entry_rel = _safe_rel_path(str(manifest.get("entry_graph", "graph.ir.json")))
        graph_path = dir_path.joinpath(*entry_rel.split("/"))
        _reject_symlink_or_reparse(graph_path, must_exist=True)
        graph = _load_json_dict(graph_path)
        _validate_ir_version_if_present(graph, graph_path)
        graph_id = str(graph.get("graph_id", ""))
        node_count = len(_expect_dict(graph.get("nodes", {}), "nodes"))
        parameter_count = len(_expect_dict(graph.get("parameters", {}), "parameters"))
        blob_count = len(_expect_dict(graph.get("blobs", {}), "blobs"))
        external_ref_count = len(_expect_dict(graph.get("external_refs", {}), "external_refs"))

    engineering_path = dir_path / "engineering.json"
    if engineering_path.exists():
        _reject_symlink_or_reparse(engineering_path, must_exist=True)
        engineering = _load_json_dict(engineering_path)
    else:
        engineering = {}

    drawings_dir = dir_path / "drawings"
    drawing_count = 0
    if drawings_dir.exists():
        _reject_symlink_or_reparse(drawings_dir, must_exist=True)
        drawing_count = sum(
            1
            for child in drawings_dir.iterdir()
            if child.is_file() and child.name.endswith(".drawing.json")
        )

    cadpack_path = dir_path / "cache" / "scene.cadpack"
    policy = str(manifest.get("kcl_projection_policy", "materialized_synced"))
    return {
        "storage": storage,
        "project_name": str(manifest.get("project_name", "")),
        "format_version": format_version,
        "ir_schema_version": ir_schema_version,
        "kcl_projection_policy": policy,
        "active_branch": "main",
        "graph_id": graph_id,
        "node_count": node_count,
        "parameter_count": parameter_count,
        "blob_count": blob_count,
        "external_ref_count": external_ref_count,
        "pmi_count": len(engineering.get("pmi", [])),
        "appearance_count": len(engineering.get("appearances", [])),
        "bom_count": len(engineering.get("bom", [])),
        "drawing_count": drawing_count,
        "has_cadpack": cadpack_path.is_file(),
        "node_brep_cache_count": 0,
        "bundled_ref_count": 0,
        "shards": shard_names,
    }


def _inspect_zip(zip_path: Path) -> dict[str, Any]:
    with zipfile.ZipFile(zip_path, "r") as archive:
        names = set(archive.namelist())
        if "manifest.json" not in names:
            raise ValueError(f"missing manifest.json in archive {zip_path}")
        manifest = json.loads(archive.read("manifest.json").decode("utf-8"))
        if not isinstance(manifest, dict):
            raise ValueError(f"expected JSON object in manifest.json of {zip_path}")
        format_version, ir_schema_version = _validate_container_versions(
            manifest.get("format_version", CONTAINER_FORMAT_VERSION),
            manifest.get("ir_schema_version", IR_SCHEMA_VERSION),
            zip_path,
        )
        entry_graph = _safe_rel_path(str(manifest.get("entry_graph", "graph.ir.json")))
        if entry_graph not in names:
            raise ValueError(f"missing {entry_graph} in archive {zip_path}")
        graph = json.loads(archive.read(entry_graph).decode("utf-8"))
        if not isinstance(graph, dict):
            raise ValueError(f"expected JSON object in {entry_graph} of {zip_path}")
        _validate_ir_version_if_present(graph, f"{zip_path}!/{entry_graph}")
        engineering = (
            json.loads(archive.read("engineering.json").decode("utf-8"))
            if "engineering.json" in names
            else {}
        )
        drawing_count = sum(
            1
            for item in names
            if item.startswith("drawings/")
            and "/" not in item.removeprefix("drawings/")
            and item.endswith(".drawing.json")
        )
        policy = str(manifest.get("kcl_projection_policy", "materialized_synced"))
        return {
            "storage": "archive",
            "project_name": str(manifest.get("project_name", "")),
            "format_version": format_version,
            "ir_schema_version": ir_schema_version,
            "kcl_projection_policy": policy,
            "active_branch": "main",
            "graph_id": str(graph.get("graph_id", "")),
            "node_count": len(_expect_dict(graph.get("nodes", {}), "nodes")),
            "parameter_count": len(_expect_dict(graph.get("parameters", {}), "parameters")),
            "blob_count": len(_expect_dict(graph.get("blobs", {}), "blobs")),
            "external_ref_count": len(_expect_dict(graph.get("external_refs", {}), "external_refs")),
            "pmi_count": len(engineering.get("pmi", [])),
            "appearance_count": len(engineering.get("appearances", [])),
            "bom_count": len(engineering.get("bom", [])),
            "drawing_count": drawing_count,
            "has_cadpack": "cache/scene.cadpack" in names,
            "node_brep_cache_count": 0,
            "bundled_ref_count": 0,
            "shards": [],
        }


def _inspect_sqlite(sqlite_path: Path) -> dict[str, Any]:
    uri = sqlite_path.resolve().as_uri() + "?mode=ro"
    conn = sqlite3.connect(uri, uri=True)
    try:
        app_id_row = conn.execute("PRAGMA application_id;").fetchone()
        app_id = int(app_id_row[0]) if app_id_row is not None else 0
        if app_id != _SQLITE_APP_ID:
            raise ValueError(
                f"SQLite file at {sqlite_path} is not an .aicad container (application_id={app_id:#x})"
            )
        meta_row = conn.execute(
            "SELECT project_name, format_version, ir_schema_version, active_branch, "
            "kcl_projection_policy, graph_id, cadpack_sha256 "
            "FROM container_meta WHERE singleton_id = 1"
        ).fetchone()
        if meta_row is None:
            raise ValueError(f"missing container_meta singleton row in {sqlite_path}")
        (
            project_name,
            format_version,
            ir_schema_version,
            active_branch,
            kcl_policy,
            graph_id,
            cadpack_sha256,
        ) = meta_row
        format_version, ir_schema_version = _validate_container_versions(
            format_version, ir_schema_version, sqlite_path
        )

        def count_where(sql: str, params: tuple[Any, ...] = ()) -> int:
            row = conn.execute(sql, params).fetchone()
            return int(row[0]) if row is not None else 0

        node_count = count_where(
            "SELECT COUNT(*) FROM graph_nodes WHERE branch_name = ?", (active_branch,)
        )
        parameter_count = count_where(
            "SELECT COUNT(*) FROM graph_parameters WHERE branch_name = ?", (active_branch,)
        )
        blob_count = count_where(
            "SELECT COUNT(*) FROM graph_blobs WHERE branch_name = ?", (active_branch,)
        )
        external_ref_count = count_where(
            "SELECT COUNT(*) FROM graph_external_refs WHERE branch_name = ?", (active_branch,)
        )
        pmi_count = count_where(
            "SELECT COUNT(*) FROM engineering_meta WHERE branch_name = ? AND category = 'pmi'",
            (active_branch,),
        )
        appearance_count = count_where(
            "SELECT COUNT(*) FROM engineering_meta WHERE branch_name = ? AND category = 'appearance'",
            (active_branch,),
        )
        bom_count = count_where(
            "SELECT COUNT(*) FROM engineering_meta WHERE branch_name = ? AND category = 'bom'",
            (active_branch,),
        )
        drawing_count = count_where(
            "SELECT COUNT(*) FROM drawing_sheets WHERE branch_name = ?", (active_branch,)
        )
        node_brep_cache_count = count_where("SELECT COUNT(*) FROM node_brep_cache")
        bundled_ref_count = count_where("SELECT COUNT(*) FROM bundled_refs")

        return {
            "storage": "sqlite",
            "project_name": str(project_name),
            "format_version": int(format_version),
            "ir_schema_version": int(ir_schema_version),
            "kcl_projection_policy": str(kcl_policy),
            "active_branch": str(active_branch),
            "graph_id": str(graph_id),
            "node_count": node_count,
            "parameter_count": parameter_count,
            "blob_count": blob_count,
            "external_ref_count": external_ref_count,
            "pmi_count": pmi_count,
            "appearance_count": appearance_count,
            "bom_count": bom_count,
            "drawing_count": drawing_count,
            "has_cadpack": cadpack_sha256 is not None,
            "node_brep_cache_count": node_brep_cache_count,
            "bundled_ref_count": bundled_ref_count,
            "shards": [],
        }
    finally:
        conn.close()


def _validate_save_inputs(project_name: str, builder: GraphBuilder) -> None:
    if not isinstance(builder, GraphBuilder):
        raise TypeError("builder must be a GraphBuilder")
    if not isinstance(project_name, str) or not project_name.strip():
        raise ValueError("project_name must contain a non-space character")


def _resolve_kernel_fingerprint(kernel_fingerprint: Mapping[str, str] | None) -> dict[str, str]:
    if kernel_fingerprint is None:
        return dict(DEFAULT_KERNEL_FINGERPRINT)
    if not isinstance(kernel_fingerprint, Mapping):
        raise TypeError("kernel_fingerprint must be a mapping with 'kernel_id' and 'kernel_version'")
    if set(kernel_fingerprint.keys()) != {"kernel_id", "kernel_version"}:
        raise ValueError(
            "kernel_fingerprint must contain exactly 'kernel_id' and 'kernel_version'"
        )
    resolved: dict[str, str] = {}
    for key in ("kernel_id", "kernel_version"):
        value = kernel_fingerprint[key]
        if not isinstance(value, str):
            raise TypeError(f"kernel_fingerprint[{key!r}] must be a string")
        if not value.strip():
            raise ValueError(f"kernel_fingerprint[{key!r}] must contain a non-space character")
        resolved[key] = value
    return resolved


def _verified_blob_payloads(
    graph: dict[str, Any],
    builder: GraphBuilder,
) -> dict[str, bytes]:
    payloads = builder.blob_payloads()
    verified: dict[str, bytes] = {}
    for entry in graph["blobs"].values():
        relative = entry["relative_path"]
        payload = payloads.get(relative)
        if payload is None:
            raise ValueError(f"missing payload for {relative}")
        if len(payload) != entry["byte_len"] or sha256_hex(payload) != entry["sha256"]:
            raise ValueError(f"blob metadata does not match the payload for {relative}")
        verified[relative] = payload
    return verified


def _verified_drawing_sheets(builder: GraphBuilder) -> dict[str, dict[str, Any]]:
    sheets = builder.drawing_sheets()
    for sheet_id, sheet in sheets.items():
        if sheet.get("sheet_id") != sheet_id or not _safe_sheet_id(sheet_id):
            raise ValueError(f"unsafe drawing sheet id {sheet_id!r}")
    return sheets


def _remove_stale_shards(dest: Path, *, keep_files: set[str]) -> None:
    graphs_dir = dest / "graphs"
    if not graphs_dir.is_dir():
        return
    keep_lower = {item.lower() for item in keep_files}
    for child in graphs_dir.iterdir():
        if child.is_file() and child.name.endswith(".ir.json"):
            if child.name.lower() not in keep_lower:
                child.unlink()


def _checked_shard_name(raw: str) -> str:
    if (
        not isinstance(raw, str)
        or not raw
        or raw in {".", ".."}
        or "/" in raw
        or "\\" in raw
        or ":" in raw
        or raw.endswith((" ", "."))
        or raw.lower() == "00_header"
        or _RESERVED_DOS_NAMES.match(raw.split(".")[0])
        or any(
            unicodedata.category(ch) == "Cc" or ch in '*?"<>|'
            for ch in raw
        )
    ):
        raise ValueError(f"unsafe graph shard name {raw!r}")
    return raw


def _safe_rel_path(raw: str) -> str:
    if not raw or "\\" in raw or ":" in raw or raw.startswith("/"):
        raise ValueError(f"unsafe container entry path {raw!r}")
    parts = raw.split("/")
    if any(part in ("", ".", "..") or part.endswith((" ", ".")) for part in parts):
        raise ValueError(f"unsafe container entry path {raw!r}")
    return "/".join(parts)


def _reject_symlink_or_reparse(path: Path, *, must_exist: bool = False) -> None:
    try:
        st = os.lstat(path)
    except FileNotFoundError:
        if must_exist:
            raise
        return
    if stat.S_ISLNK(st.st_mode):
        raise ValueError(f"unsafe symlink path {path}")
    file_attrs = getattr(st, "st_file_attributes", 0)
    reparse_flag = getattr(stat, "FILE_ATTRIBUTE_REPARSE_POINT", 0x400)
    if file_attrs & reparse_flag:
        raise ValueError(f"unsafe reparse point path {path}")


def _load_json_dict(path: Path) -> dict[str, Any]:
    payload = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(payload, dict):
        raise ValueError(f"expected JSON object in {path}")
    return payload


def _expect_dict(value: Any, label: str) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise ValueError(f"{label} must be an object")
    return value


def _pretty(value: object) -> str:
    return json.dumps(value, indent=2, sort_keys=True, allow_nan=False) + "\n"


def _write_text(path: Path, text: str) -> None:
    path.write_text(text, encoding="utf-8", newline="\n")


def _safe_sheet_id(sheet_id: str) -> bool:
    return (
        isinstance(sheet_id, str)
        and bool(sheet_id)
        and sheet_id not in {".", ".."}
        and "/" not in sheet_id
        and "\\" not in sheet_id
        and not sheet_id.endswith((" ", "."))
    )


def _write_blob(dest: Path, relative_path: str, payload: bytes) -> None:
    parts = relative_path.split("/")
    if (
        len(parts) < 3
        or parts[0] != "blobs"
        or any(part in ("", ".", "..") for part in parts)
        or "\\" in relative_path
    ):
        raise ValueError(f"unsafe blob path {relative_path!r}")
    target = dest.joinpath(*parts)
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_bytes(payload)
