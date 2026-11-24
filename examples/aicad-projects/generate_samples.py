#!/usr/bin/env python3
"""Deterministically generate or verify the curated V2 `.aicad` sample projects.

Run without flags to regenerate `examples/aicad-projects/*.aicad`.
Run with `--check` to verify the committed directories match the generated output byte-for-byte.
"""

from __future__ import annotations

import argparse
import shutil
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent
REPO_ROOT = ROOT.parents[1]
SDK_SRC = REPO_ROOT / "packages" / "python-sdk" / "src"
if SDK_SRC.is_dir() and str(SDK_SRC) not in sys.path:
    sys.path.insert(0, str(SDK_SRC))

from aicad import EngineeringDocument, mm  # noqa: E402


def build_mounting_plate() -> EngineeringDocument:
    doc = EngineeringDocument("MountingPlate", graph_id="mounting-plate")
    doc.set_design_contract(
        "MountingPlate",
        summary="Parametric aluminum mounting plate with center pilot bore and 4 M6 ISO 4762 counterbored corner holes",
        material="Al-6061-T6",
        manufacturing_method="CNC_3AXIS_MILLING",
    )
    doc.upsert_parameter("plate_width", mm(120.0))
    doc.upsert_parameter("plate_depth", mm(80.0))
    doc.upsert_parameter("plate_thickness", mm(12.0))
    doc.upsert_parameter("center_bore_d", mm(28.0))

    doc.create_box(
        "BasePlate",
        "plate_width",
        "plate_depth",
        "plate_thickness",
        node_id="base_plate",
    )
    current = doc.create_hole(
        "CenterPilotBore",
        "base_plate",
        "center_bore_d",
        node_id="center_bore",
        origin_mm=(60.0, 40.0, 12.0),
        axis=(0.0, 0.0, -1.0),
        depth="through_all",
    )
    corners = [
        ("hole_fl", "MountHoleFL", (15.0, 15.0, 12.0)),
        ("hole_fr", "MountHoleFR", (105.0, 15.0, 12.0)),
        ("hole_rl", "MountHoleRL", (15.0, 65.0, 12.0)),
        ("hole_rr", "MountHoleRR", (105.0, 65.0, 12.0)),
    ]
    for nid, name, origin in corners:
        current = doc.create_std_fastener_hole(
            name,
            current,
            "iso4762_counterbore",
            "M6",
            node_id=nid,
            clearance_class="normal",
            depth="through_all",
            origin_mm=origin,
            axis=(0.0, 0.0, -1.0),
        )
    return doc


def build_flanged_collar() -> EngineeringDocument:
    doc = EngineeringDocument("FlangedCollar", graph_id="flanged-collar")
    doc.set_design_contract(
        "FlangedCollar",
        summary="Stepped flanged bearing collar with concentric through bore",
        material="Al-6061-T6",
        manufacturing_method="CNC_3AXIS_MILLING",
    )
    doc.upsert_parameter("flange_radius", mm(36.0))
    doc.upsert_parameter("flange_height", mm(10.0))
    doc.upsert_parameter("hub_radius", mm(22.0))
    doc.upsert_parameter("hub_height", mm(24.0))
    doc.upsert_parameter("bore_diameter", mm(20.0))

    doc.create_cylinder(
        "FlangeDisc",
        "flange_radius",
        "flange_height",
        node_id="flange_disc",
        origin_mm=(0.0, 0.0, 0.0),
    )
    doc.create_cylinder(
        "CenterHub",
        "hub_radius",
        "hub_height",
        node_id="center_hub",
        origin_mm=(0.0, 0.0, 0.0),
    )
    doc.boolean_solids(
        "CollarBody",
        "union",
        "flange_disc",
        ["center_hub"],
        node_id="collar_body",
    )
    doc.create_hole(
        "ShaftBore",
        "collar_body",
        "bore_diameter",
        node_id="shaft_bore",
        origin_mm=(0.0, 0.0, 24.0),
        axis=(0.0, 0.0, -1.0),
        depth="through_all",
    )
    return doc


SAMPLES = {
    "01_mounting_plate.aicad": build_mounting_plate,
    "02_flanged_collar.aicad": build_flanged_collar,
}


def snapshot_tree(root_dir: Path) -> dict[str, bytes]:
    result: dict[str, bytes] = {}
    for path in sorted(root_dir.rglob("*")):
        if path.is_file():
            rel = path.relative_to(root_dir).as_posix()
            raw = path.read_bytes()
            if path.suffix.lower() in {".json", ".kcl", ".md"}:
                raw = raw.replace(b"\r\n", b"\n")
            result[rel] = raw
    return result


def generate_into(target_root: Path) -> None:
    target_root.mkdir(parents=True, exist_ok=True)
    for folder_name, builder_fn in SAMPLES.items():
        dest = target_root / folder_name
        if dest.exists():
            shutil.rmtree(dest)
        doc = builder_fn()
        doc.save(dest)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--check",
        action="store_true",
        help="Verify committed sample directories match generated output without writing.",
    )
    args = parser.parse_args(argv)

    if args.check:
        with tempfile.TemporaryDirectory(prefix="aicad-samples-check-") as tmp:
            tmp_root = Path(tmp)
            generate_into(tmp_root)
            mismatches: list[str] = []
            for folder_name in SAMPLES:
                expected = snapshot_tree(tmp_root / folder_name)
                actual_dir = ROOT / folder_name
                if not actual_dir.is_dir():
                    mismatches.append(f"missing sample directory: {folder_name}")
                    continue
                actual = snapshot_tree(actual_dir)
                if expected != actual:
                    mismatches.append(
                        f"sample directory differs from generator: {folder_name} "
                        f"(expected {sorted(expected)}, actual {sorted(actual)})"
                    )
            if mismatches:
                for item in mismatches:
                    print(item, file=sys.stderr)
                return 1
        print(f"Verified {len(SAMPLES)} .aicad sample projects.")
        return 0

    generate_into(ROOT)
    print(f"Generated {len(SAMPLES)} .aicad sample projects in {ROOT}.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
