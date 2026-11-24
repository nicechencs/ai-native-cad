#!/usr/bin/env python3
"""Generate Tauri v2 `latest.json` updater manifest from signed release artifacts."""

from __future__ import annotations

import argparse
import datetime as dt
import json
import re
from pathlib import Path
from urllib.parse import quote

SEMVER_RE = re.compile(r"^[0-9]+\.[0-9]+\.[0-9]+(?:[-+][0-9A-Za-z.-]+)*$")


def normalize_version(raw: str) -> str:
    cleaned = raw.strip()
    if cleaned.startswith("v"):
        cleaned = cleaned[1:]
    if not SEMVER_RE.match(cleaned):
        raise ValueError(f"Invalid semantic version: {raw!r}")
    return cleaned


def infer_platform_key(artifact_name: str) -> str | None:
    """Map a Tauri v2 updater binary filename to its Tauri platform triplet."""
    lower = artifact_name.lower()
    if lower.endswith("-setup.exe") or lower.endswith(".msi") or lower.endswith(".nsis.zip") or lower.endswith(".msi.zip"):
        if "arm64" in lower or "aarch64" in lower:
            return "windows-aarch64"
        return "windows-x86_64"
    if lower.endswith(".app.tar.gz"):
        if ("x86_64" in lower or "x64" in lower or "intel" in lower) and "aarch64" not in lower and "arm64" not in lower:
            return "darwin-x86_64"
        return "darwin-aarch64"
    if lower.endswith(".appimage") or lower.endswith(".appimage.tar.gz"):
        if "arm64" in lower or "aarch64" in lower:
            return "linux-aarch64"
        return "linux-x86_64"
    return None


def build_updater_manifest(
    artifacts_dir: Path,
    *,
    version: str,
    base_url: str,
    notes: str = "",
    pub_date: str | None = None,
) -> dict:
    resolved_version = normalize_version(version)
    cleaned_base = base_url.strip().rstrip("/")
    if not cleaned_base.startswith("https://"):
        raise ValueError(f"Updater base URL must use https://, got: {base_url!r}")

    timestamp = pub_date.strip() if pub_date else dt.datetime.now(dt.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
    platforms: dict[str, dict[str, str]] = {}

    for sig_path in sorted(artifacts_dir.rglob("*.sig")):
        if not sig_path.is_file():
            continue
        artifact_name = sig_path.name[: -len(".sig")]
        artifact_path = sig_path.with_name(artifact_name)
        if not artifact_path.is_file():
            raise FileNotFoundError(
                f"Found signature {sig_path} without matching updater binary {artifact_path}"
            )
        platform_key = infer_platform_key(artifact_name)
        if platform_key is None:
            continue
        signature = sig_path.read_text(encoding="utf-8").strip()
        if not signature:
            raise ValueError(f"Empty signature file: {sig_path}")
        if platform_key in platforms:
            raise ValueError(
                f"Duplicate updater artifact for platform {platform_key}: {artifact_name}"
            )
        platforms[platform_key] = {
            "signature": signature,
            "url": f"{cleaned_base}/{quote(artifact_name)}",
        }

    if not platforms:
        raise FileNotFoundError(
            f"No signed Tauri updater artifacts (*.sig) found under {artifacts_dir}"
        )

    return {
        "version": resolved_version,
        "notes": notes.strip() or f"AI-Native CAD Studio v{resolved_version}",
        "pub_date": timestamp,
        "platforms": dict(sorted(platforms.items())),
    }


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Generate Tauri v2 latest.json updater manifest.")
    parser.add_argument("--artifacts-dir", type=Path, required=True)
    parser.add_argument("--version", required=True, help="Release version (e.g. 0.2.0 or v0.2.0)")
    parser.add_argument(
        "--base-url",
        required=True,
        help="HTTPS release download base URL (e.g. https://github.com/owner/repo/releases/download/v0.2.0)",
    )
    parser.add_argument("--notes", default="", help="Optional release notes text")
    parser.add_argument("--pub-date", default=None, help="Optional RFC 3339 UTC timestamp")
    parser.add_argument("--out", type=Path, required=True, help="Output path for latest.json")
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    manifest = build_updater_manifest(
        args.artifacts_dir,
        version=args.version,
        base_url=args.base_url,
        notes=args.notes,
        pub_date=args.pub_date,
    )
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
    print(f"Wrote Tauri updater manifest ({len(manifest['platforms'])} platforms): {args.out}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
