#!/usr/bin/env python3
"""Stage bundled runtime assets and generate a Tauri v2 release config overlay.

This script prepares `apps/desktop/src-tauri/.bundle-stage/` so `tauri build`
produces formal cross-platform installers (Windows NSIS, macOS .app/.dmg/.app.tar.gz,
Linux AppImage) that include:
  - `cad-cli` (CLI + daemon)
  - `services/cad-agent/` (AI multi-agent orchestrator)
  - `occt_bridge` + OCCT 7.9+ LGPL-2.1 dynamic libraries (when --with-occt)
  - `plugins.updater` public key & GitHub Releases endpoint configuration

It can also normalize built Tauri installer and `.sig` filenames (removing spaces
and tagging architecture) via `--collect-bundles` before uploading to GitHub Releases.
"""

from __future__ import annotations

import argparse
import json
import os
import platform
import re
import shutil
import sys
from pathlib import Path
from typing import Iterable

DEFAULT_GITHUB_REPO = "nicechencs/ai-native-cad"
SEMVER_RE = re.compile(r"^[0-9]+\.[0-9]+\.[0-9]+(?:[-+][0-9A-Za-z.-]+)*$")


def normalize_platform(raw: str | None) -> str:
    if raw:
        lowered = raw.strip().lower()
        if lowered in {"windows", "win", "win32"}:
            return "windows"
        if lowered in {"macos", "darwin", "osx"}:
            return "macos"
        if lowered in {"linux"}:
            return "linux"
        raise ValueError(f"Unsupported platform: {raw}")
    system = platform.system().lower()
    if system == "windows":
        return "windows"
    if system == "darwin":
        return "macos"
    if system == "linux":
        return "linux"
    raise ValueError(f"Unsupported host platform: {platform.system()}")


def normalize_version(raw: str) -> str:
    cleaned = raw.strip()
    if cleaned.startswith("v"):
        cleaned = cleaned[1:]
    if not SEMVER_RE.match(cleaned):
        raise ValueError(f"Invalid semantic version: {raw!r}")
    return cleaned


def read_repo_version(repo_root: Path) -> str:
    pkg = json.loads((repo_root / "package.json").read_text(encoding="utf-8"))
    return normalize_version(str(pkg["version"]))


def resolve_cad_cli(repo_root: Path, profile: str, target_os: str, explicit: Path | None) -> Path:
    if explicit is not None:
        if not explicit.is_file():
            raise FileNotFoundError(f"cad-cli binary not found: {explicit}")
        return explicit
    exe = ".exe" if target_os == "windows" else ""
    candidate = repo_root / "target" / profile / f"cad-cli{exe}"
    if not candidate.is_file():
        raise FileNotFoundError(
            f"Missing cad-cli binary at {candidate}. Build cad-cli before staging Tauri bundle."
        )
    return candidate


def resolve_occt_libraries(
    repo_root: Path,
    profile: str,
    target_os: str,
    bridge_lib_dir: Path | None = None,
) -> tuple[Path, list[Path]]:
    if target_os == "windows":
        bridge_name = "occt_bridge.dll"
        runtime_dirs = [
            repo_root / "cpp" / "occt-prebuilt" / "Library" / "bin",
            repo_root / "cpp" / "occt-prebuilt",
        ]
        lib_matcher = lambda p: p.is_file() and p.name.lower().endswith(".dll")
    elif target_os == "macos":
        bridge_name = "libocct_bridge.dylib"
        runtime_dirs = [repo_root / "cpp" / "occt-prebuilt" / "lib"]
        lib_matcher = lambda p: p.is_file() and p.name.endswith(".dylib")
    else:
        bridge_name = "libocct_bridge.so"
        runtime_dirs = [repo_root / "cpp" / "occt-prebuilt" / "lib"]
        lib_matcher = lambda p: p.is_file() and (p.name.endswith(".so") or ".so." in p.name)

    profile_title = "Release" if profile == "release" else "Debug"
    candidate_dirs: list[Path] = []
    if bridge_lib_dir is not None:
        candidate_dirs.append(bridge_lib_dir)
    env_dir = os.environ.get("OCCT_BRIDGE_LIB_DIR", "").strip()
    if env_dir:
        candidate_dirs.append(Path(env_dir))
    candidate_dirs.extend(
        [
            repo_root / "build" / "occt-bridge" / profile_title,
            repo_root / "build" / "occt-bridge" / profile,
            repo_root / "build" / "occt-bridge" / "Release",
            repo_root / "build" / "occt-bridge" / "RelWithDebInfo",
            repo_root / "build" / "occt-bridge" / "Debug",
            repo_root / "build" / "occt-bridge",
            repo_root / "cpp" / "occt-bridge" / "build" / profile_title,
            repo_root / "cpp" / "occt-bridge" / "build" / "Release",
            repo_root / "cpp" / "occt-bridge" / "build",
            *runtime_dirs,
        ]
    )

    bridge_path: Path | None = None
    for directory in candidate_dirs:
        probe = directory / bridge_name
        if probe.is_file():
            bridge_path = probe
            break
    if bridge_path is None:
        raise FileNotFoundError(
            f"OCCT bridge library {bridge_name} not found. Build cpp/occt-bridge or set OCCT_BRIDGE_LIB_DIR."
        )

    runtime_libs: list[Path] = []
    seen_names: set[str] = {bridge_name.lower()}
    for directory in runtime_dirs:
        if not directory.is_dir():
            continue
        for entry in sorted(directory.iterdir(), key=lambda item: item.name):
            if not lib_matcher(entry):
                continue
            key = entry.name.lower() if target_os == "windows" else entry.name
            if key in seen_names:
                continue
            seen_names.add(key)
            runtime_libs.append(entry)

    if not runtime_libs:
        joined = ", ".join(str(d) for d in runtime_dirs)
        raise FileNotFoundError(f"No OCCT runtime libraries found under: {joined}")
    return bridge_path, runtime_libs


def copy_cad_agent(repo_root: Path, dest_root: Path) -> Path:
    src_pkg = repo_root / "services" / "cad-agent" / "src" / "cad_agent"
    if not (src_pkg / "cli.py").is_file():
        raise FileNotFoundError(f"cad_agent.cli source not found at {src_pkg / 'cli.py'}")
    dest_pkg = dest_root / "services" / "cad-agent" / "src" / "cad_agent"
    shutil.copytree(
        src_pkg,
        dest_pkg,
        ignore=shutil.ignore_patterns("__pycache__", "*.pyc", "*.pyo", ".pytest_cache"),
        dirs_exist_ok=True,
    )
    pyproject = repo_root / "services" / "cad-agent" / "pyproject.toml"
    if pyproject.is_file():
        shutil.copy2(pyproject, dest_root / "services" / "cad-agent" / "pyproject.toml")
    return dest_pkg


def copy_file_dereference(src: Path, dst: Path) -> None:
    dst.parent.mkdir(parents=True, exist_ok=True)
    # Follow symlinks so bundled shared libraries inside installers are concrete files.
    shutil.copy2(src.resolve(), dst)


def stage_tauri_bundle(
    repo_root: Path,
    *,
    target_os: str,
    profile: str = "release",
    with_occt: bool = True,
    cad_cli_path: Path | None = None,
    bridge_lib_dir: Path | None = None,
    version: str | None = None,
    pubkey: str = "",
    endpoint: str = "",
    github_repo: str = DEFAULT_GITHUB_REPO,
    create_updater_artifacts: bool = False,
    skip_before_build: bool = False,
) -> tuple[Path, dict]:
    tauri_dir = repo_root / "apps" / "desktop" / "src-tauri"
    stage_dir = tauri_dir / ".bundle-stage"
    if stage_dir.exists():
        shutil.rmtree(stage_dir)
    resources_dir = stage_dir / "resources"
    resources_dir.mkdir(parents=True, exist_ok=True)

    resolved_version = normalize_version(version) if version else read_repo_version(repo_root)
    cli_src = resolve_cad_cli(repo_root, profile, target_os, cad_cli_path)
    copy_file_dereference(cli_src, resources_dir / cli_src.name)
    copy_cad_agent(repo_root, resources_dir)

    linux_appimage_files: dict[str, str] = {}
    staged_libs: list[str] = []
    if with_occt:
        bridge_src, runtime_libs = resolve_occt_libraries(
            repo_root, profile, target_os, bridge_lib_dir=bridge_lib_dir
        )
        all_libs = [bridge_src, *runtime_libs]
        if target_os == "linux":
            linux_lib_dir = stage_dir / "linux-lib"
            linux_lib_dir.mkdir(parents=True, exist_ok=True)
            for lib in all_libs:
                copy_file_dereference(lib, linux_lib_dir / lib.name)
                linux_appimage_files[f"/usr/lib/{lib.name}"] = f".bundle-stage/linux-lib/{lib.name}"
                staged_libs.append(lib.name)
        else:
            for lib in all_libs:
                copy_file_dereference(lib, resources_dir / lib.name)
                staged_libs.append(lib.name)

    if target_os == "windows":
        targets = ["nsis"]
    elif target_os == "macos":
        targets = ["app", "dmg"]
    else:
        targets = ["appimage"]

    resolved_endpoint = endpoint.strip() or (
        f"https://github.com/{github_repo.strip() or DEFAULT_GITHUB_REPO}"
        "/releases/latest/download/latest.json"
    )
    resolved_pubkey = pubkey.strip()

    bundle_cfg: dict = {
        "active": True,
        "targets": targets,
        "createUpdaterArtifacts": bool(create_updater_artifacts),
        "resources": {
            ".bundle-stage/resources/": "",
        },
    }
    if target_os == "linux" and linux_appimage_files:
        bundle_cfg["linux"] = {
            "appimage": {
                "files": linux_appimage_files,
            }
        }

    overlay: dict = {
        "version": resolved_version,
        "bundle": bundle_cfg,
        "plugins": {
            "updater": {
                "pubkey": resolved_pubkey,
                "endpoints": [resolved_endpoint],
                "windows": {
                    "installMode": "passive",
                },
            }
        },
    }
    if skip_before_build:
        overlay["build"] = {"beforeBuildCommand": ""}

    config_path = stage_dir / "tauri.release.conf.json"
    config_path.write_text(json.dumps(overlay, indent=2) + "\n", encoding="utf-8")

    summary = {
        "platform": target_os,
        "version": resolved_version,
        "withOcct": with_occt,
        "createUpdaterArtifacts": bool(create_updater_artifacts),
        "pubkeyConfigured": bool(resolved_pubkey),
        "endpoint": resolved_endpoint,
        "targets": targets,
        "stagedLibraryCount": len(staged_libs),
        "configPath": str(config_path),
    }
    (stage_dir / "stage-summary.json").write_text(json.dumps(summary, indent=2) + "\n", encoding="utf-8")
    return config_path, summary


def sanitize_release_filename(name: str, target_name: str, version: str) -> str:
    """Produce a space-free GitHub Release filename with clear platform tag."""
    safe = re.sub(r"\s+", "-", name.strip())
    lower = safe.lower()
    if lower.endswith(".app.tar.gz.sig"):
        stem = safe[: -len(".app.tar.gz.sig")]
        if target_name.lower() not in lower:
            return f"{stem}_{version}_{target_name}.app.tar.gz.sig"
        return safe
    if lower.endswith(".app.tar.gz"):
        stem = safe[: -len(".app.tar.gz")]
        if target_name.lower() not in lower:
            return f"{stem}_{version}_{target_name}.app.tar.gz"
        return safe
    return safe


def collect_tauri_bundles(
    bundle_root: Path,
    out_dir: Path,
    *,
    target_name: str,
    version: str,
) -> list[Path]:
    """Copy built installers and updater `.sig` files with GitHub-safe filenames."""
    resolved_version = normalize_version(version)
    out_dir.mkdir(parents=True, exist_ok=True)
    patterns: Iterable[str] = (
        "nsis/*-setup.exe",
        "nsis/*-setup.exe.sig",
        "msi/*.msi",
        "msi/*.msi.sig",
        "dmg/*.dmg",
        "macos/*.app.tar.gz",
        "macos/*.app.tar.gz.sig",
        "appimage/*.AppImage",
        "appimage/*.AppImage.sig",
    )
    copied: list[Path] = []
    seen: set[Path] = set()
    for pattern in patterns:
        for src in sorted(bundle_root.glob(pattern)):
            if not src.is_file() or src in seen:
                continue
            seen.add(src)
            dst_name = sanitize_release_filename(src.name, target_name, resolved_version)
            dst = out_dir / dst_name
            shutil.copy2(src, dst)
            copied.append(dst)
    if not copied:
        raise FileNotFoundError(f"No Tauri installer bundles found under {bundle_root}")
    return copied


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Stage assets and config for Tauri v2 release installers.")
    parser.add_argument("--repo-root", type=Path, default=Path(__file__).resolve().parent.parent)
    parser.add_argument("--platform", default=None, help="Target OS: windows, macos, linux (default: host)")
    parser.add_argument("--profile", choices=("release", "debug"), default="release")
    parser.add_argument("--with-occt", dest="with_occt", action="store_true", default=True)
    parser.add_argument("--without-occt", dest="with_occt", action="store_false")
    parser.add_argument("--cad-cli", type=Path, default=None, help="Explicit path to cad-cli binary")
    parser.add_argument("--bridge-lib-dir", type=Path, default=None, help="Explicit occt_bridge library directory")
    parser.add_argument("--version", default=None, help="Override bundle semver (e.g. 0.2.0 or v0.2.0)")
    parser.add_argument(
        "--pubkey",
        default=os.environ.get("TAURI_SIGNING_PUBLIC_KEY", ""),
        help="Updater minisign public key (default: TAURI_SIGNING_PUBLIC_KEY env)",
    )
    parser.add_argument("--endpoint", default="", help="Explicit updater latest.json URL")
    parser.add_argument(
        "--github-repo",
        default=os.environ.get("GITHUB_REPOSITORY", DEFAULT_GITHUB_REPO),
        help="owner/repo used to derive default GitHub Releases latest.json endpoint",
    )
    parser.add_argument(
        "--create-updater-artifacts",
        action="store_true",
        default=False,
        help="Enable Tauri updater artifact + .sig generation",
    )
    parser.add_argument(
        "--skip-before-build",
        action="store_true",
        default=False,
        help="Clear build.beforeBuildCommand in the overlay when frontend dist is already built",
    )
    parser.add_argument(
        "--collect-from",
        type=Path,
        default=None,
        help="Collect and normalize built Tauri bundles from this bundle directory instead of staging",
    )
    parser.add_argument("--collect-to", type=Path, default=None, help="Destination directory for --collect-from")
    parser.add_argument("--target-name", default="", help="Target identifier (e.g. macos-arm64) for --collect-from")
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    if args.collect_from is not None:
        if args.collect_to is None:
            raise SystemExit("--collect-to is required when --collect-from is set")
        version = args.version or read_repo_version(args.repo_root.resolve())
        copied = collect_tauri_bundles(
            args.collect_from,
            args.collect_to,
            target_name=args.target_name or normalize_platform(args.platform),
            version=version,
        )
        print(json.dumps({"collected": [str(p) for p in copied]}, indent=2))
        return 0

    target_os = normalize_platform(args.platform)
    _, summary = stage_tauri_bundle(
        args.repo_root.resolve(),
        target_os=target_os,
        profile=args.profile,
        with_occt=args.with_occt,
        cad_cli_path=args.cad_cli,
        bridge_lib_dir=args.bridge_lib_dir,
        version=args.version,
        pubkey=args.pubkey,
        endpoint=args.endpoint,
        github_repo=args.github_repo,
        create_updater_artifacts=args.create_updater_artifacts,
        skip_before_build=args.skip_before_build,
    )
    print(json.dumps(summary, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
