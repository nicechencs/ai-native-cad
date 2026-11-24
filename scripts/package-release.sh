#!/usr/bin/env bash
set -euo pipefail

fail() {
    printf 'Error: %s\n' "$*" >&2
    exit 1
}

out_dir=dist/release
profile=release
skip_frontend=false
skip_build=false
cli_only=false
with_occt=true
occt_flag_set=false
no_zip=false
dry_run=false

while (($#)); do
    case "$1" in
        --out-dir)
            (($# >= 2)) && [[ -n "$2" ]] || fail "--out-dir requires a path"
            out_dir=$2
            shift 2
            ;;
        --profile)
            (($# >= 2)) || fail "--profile requires release or debug"
            profile=$2
            shift 2
            ;;
        --skip-frontend) skip_frontend=true; shift ;;
        --skip-build) skip_build=true; shift ;;
        --cli-only) cli_only=true; shift ;;
        --with-occt)
            $occt_flag_set && fail "Pass only one of --with-occt or --without-occt."
            with_occt=true
            occt_flag_set=true
            shift
            ;;
        --without-occt)
            $occt_flag_set && fail "Pass only one of --with-occt or --without-occt."
            with_occt=false
            occt_flag_set=true
            shift
            ;;
        --no-zip) no_zip=true; shift ;;
        --dry-run) dry_run=true; shift ;;
        *) fail "Unknown option: $1" ;;
    esac
done
case "$profile" in
    release|debug) ;;
    *) fail "Invalid profile: $profile (expected release or debug)" ;;
esac
case "$out_dir" in
    /*|[a-zA-Z]:/*|[a-zA-Z]:\\*) ;;
    *) out_dir="$PWD/$out_dir" ;;
esac

repo_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cd -- "$repo_root"
version=$(node -p "require('./package.json').version")
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+([-+][0-9A-Za-z.-]+)*$ ]] || fail "Invalid package version: $version"

exe_suffix=
runtime_dir=cpp/occt-prebuilt/lib
case "$(uname -s)" in
    Linux) platform=linux; bridge_name=libocct_bridge.so; library_glob='*.so*' ;;
    Darwin) platform=macos; bridge_name=libocct_bridge.dylib; library_glob='*.dylib' ;;
    MINGW*|MSYS*|CYGWIN*)
        platform=windows
        exe_suffix=.exe
        bridge_name=occt_bridge.dll
        library_glob='*.dll'
        runtime_dir=cpp/occt-prebuilt/Library/bin
        ;;
    *) fail "Unsupported platform: $(uname -s)" ;;
esac
case "$(uname -m)" in
    x86_64|amd64) arch=x64 ;;
    aarch64|arm64) arch=arm64 ;;
    *) fail "Unsupported architecture: $(uname -m)" ;;
esac

bundle_name="ai-native-cad-v$version-$platform-$arch"
bundle_path="$out_dir/$bundle_name"
archive_path=
if ! $no_zip; then
    archive_path="$bundle_path.tar.gz"
fi

desktop_manifest=apps/desktop/src-tauri/Cargo.toml
frontend_index=apps/studio-ui/dist/index.html
frontend_cmd=(pnpm --filter @ai-native-cad/studio-ui build)
cli_cmd=(cargo build -p cad-cli)
desktop_cmd=(cargo build --manifest-path "$desktop_manifest" --features custom-protocol)
if [[ "$profile" == release ]]; then
    cli_cmd+=(--release)
    desktop_cmd+=(--release)
fi
if $with_occt; then
    cli_cmd+=(--features occt-ffi)
    desktop_cmd+=(--features occt-ffi)
fi

binary_sources=("target/$profile/cad-cli$exe_suffix")
if ! $cli_only; then
    binary_sources+=("apps/desktop/src-tauri/target/$profile/ai-native-cad-desktop$exe_suffix")
fi

if $dry_run; then
    steps=()
    if ! $skip_build; then
        if ! $cli_only && ! $skip_frontend; then
            steps+=("${frontend_cmd[*]}" "Verify $frontend_index")
        fi
        steps+=("${cli_cmd[*]}")
        if ! $cli_only; then
            steps+=("${desktop_cmd[*]}")
        fi
    fi
    steps+=("Stage ${binary_sources[*]} in $bundle_path")
    if $with_occt; then
        if [[ "$platform" == windows ]]; then
            steps+=("Stage available $bridge_name, $runtime_dir/$library_glob, and cpp/occt-prebuilt/$library_glob")
        else
            steps+=("Stage available $bridge_name and $runtime_dir/$library_glob")
        fi
    fi
    steps+=("Write README-PORTABLE.txt, BUILD-MANIFEST.json and SHA256SUMS.txt")
    if [[ -n "$archive_path" ]]; then
        steps+=("Create tar.gz archive at $archive_path")
    fi
    printf '[DRY-RUN] %s\n' "${steps[@]}"
    node -e '
        const [version, profile, bundle, archive, ...steps] = process.argv.slice(1);
        console.log(JSON.stringify({version, profile, dryRun: true, bundle, archive: archive || null, steps}, null, 2));
    ' -- "$version" "$profile" "$bundle_path" "$archive_path" "${steps[@]}"
    exit 0
fi

run_build() {
    printf '[BUILD] %s\n' "$*"
    "$@"
}

if $with_occt && ! $skip_build && [[ -z "${OCCT_BRIDGE_LIB_DIR:-}" ]]; then
    bridge_lib_candidates=(
        "build/occt-bridge/Release"
        "build/occt-bridge/RelWithDebInfo"
        "build/occt-bridge/MinSizeRel"
        "build/occt-bridge/Debug"
        "build/occt-bridge/$profile"
        "build/occt-bridge"
        "cpp/occt-bridge/build/Release"
        "cpp/occt-bridge/build/RelWithDebInfo"
        "cpp/occt-bridge/build/MinSizeRel"
        "cpp/occt-bridge/build/Debug"
        "cpp/occt-bridge/build/$profile"
        "cpp/occt-bridge/build"
    )
    for directory in "${bridge_lib_candidates[@]}"; do
        if [[ -f "$directory/occt_bridge.lib" || -f "$directory/libocct_bridge.so" || -f "$directory/libocct_bridge.dylib" ]]; then
            OCCT_BRIDGE_LIB_DIR=$(cd -- "$directory" && pwd)
            export OCCT_BRIDGE_LIB_DIR
            printf '[OCCT] OCCT_BRIDGE_LIB_DIR=%s\n' "$OCCT_BRIDGE_LIB_DIR"
            break
        fi
    done
    [[ -n "${OCCT_BRIDGE_LIB_DIR:-}" ]] || fail "OCCT bridge library not found. Build cpp/occt-bridge or set OCCT_BRIDGE_LIB_DIR."
fi

if ! $skip_build; then
    if ! $cli_only && ! $skip_frontend; then
        run_build "${frontend_cmd[@]}"
        [[ -f "$frontend_index" ]] || fail "Frontend build did not produce $frontend_index"
    fi
    run_build "${cli_cmd[@]}"
    if ! $cli_only; then
        run_build "${desktop_cmd[@]}"
    fi
fi

for source in "${binary_sources[@]}"; do
    [[ -f "$source" ]] || fail "Missing binary: $source. Build the selected profile before using --skip-build."
done

if $with_occt; then
    profile_title=Release
    if [[ "$profile" == debug ]]; then
        profile_title=Debug
    fi
    bridge_dirs=("${OCCT_BRIDGE_LIB_DIR:-}" "build/occt-bridge/$profile" "build/occt-bridge/$profile_title"
        "build/occt-bridge/Release" "build/occt-bridge/RelWithDebInfo" "build/occt-bridge/MinSizeRel"
        "build/occt-bridge/Debug" "build/occt-bridge" "target/$profile"
        "apps/desktop/src-tauri/target/$profile" "cpp/occt-bridge/build/$profile"
        "cpp/occt-bridge/build/Release" "cpp/occt-bridge/build/RelWithDebInfo"
        "cpp/occt-bridge/build/MinSizeRel" "cpp/occt-bridge/build/Debug"
        "cpp/occt-bridge/build" "$runtime_dir")
    for directory in "${bridge_dirs[@]}"; do
        if [[ -n "$directory" && -f "$directory/$bridge_name" ]]; then
            binary_sources+=("$directory/$bridge_name")
            break
        fi
    done
    runtime_dirs=("$runtime_dir")
    if [[ "$platform" == windows ]]; then
        runtime_dirs+=("cpp/occt-prebuilt")
    fi
    staged_libs=()
    for dir in "${runtime_dirs[@]}"; do
        for library in "$dir"/$library_glob; do
            lib_name="${library##*/}"
            if [[ -f "$library" && "$lib_name" != "$bridge_name" ]]; then
                already_staged=false
                for seen in ${staged_libs[@]+"${staged_libs[@]}"}; do
                    if [[ "$seen" == "$lib_name" ]]; then
                        already_staged=true
                        break
                    fi
                done
                if ! $already_staged; then
                    staged_libs+=("$lib_name")
                    binary_sources+=("$library")
                fi
            fi
        done
    done
    bridge_found=false
    for source in "${binary_sources[@]}"; do
        if [[ "${source##*/}" == "$bridge_name" ]]; then
            bridge_found=true
            break
        fi
    done
    $bridge_found || fail "OCCT bridge library $bridge_name was not found. Build cpp/occt-bridge or set OCCT_BRIDGE_LIB_DIR."
    ((${#staged_libs[@]} > 0)) || fail "No OCCT runtime libraries found under $runtime_dir."
fi

mkdir -p -- "$out_dir"
work_dir=$(mktemp -d "$out_dir/.package-XXXXXXXX")
trap 'rm -rf -- "$work_dir"' EXIT
stage_dir="$work_dir/$bundle_name"
mkdir -- "$stage_dir"
binary_names=()
for source in "${binary_sources[@]}"; do
    cp -pL -- "$source" "$stage_dir/"
    binary_names+=("${source##*/}")
done

{
    printf 'AI-Native CAD %s portable bundle\n\n' "$version"
    if ! $cli_only; then
        printf 'Run ./ai-native-cad-desktop%s to start the desktop workbench.\n' "$exe_suffix"
    fi
    printf 'Run ./cad-cli%s --help to list CLI commands.\n' "$exe_suffix"
    printf 'Run ./cad-cli%s daemon to start the CLI daemon.\n' "$exe_suffix"
    if $with_occt; then
        case "$platform" in
            linux) printf 'Set LD_LIBRARY_PATH to the bundle directory when using the bundled OCCT libraries.\n' ;;
            macos) printf 'Set DYLD_LIBRARY_PATH to the bundle directory when using the bundled OCCT libraries.\n' ;;
        esac
    fi
    printf 'Use SHA256SUMS.txt to verify the packaged binaries.\n'
} > "$stage_dir/README-PORTABLE.txt"

node --input-type=module -e '
    import { createHash } from "node:crypto";
    import { createReadStream } from "node:fs";
    import { stat, writeFile } from "node:fs/promises";
    import { join } from "node:path";
    const [directory, version, profile, ...names] = process.argv.slice(1);
    const binaries = [];
    for (const name of names) {
        const path = join(directory, name);
        const { size: sizeBytes } = await stat(path);
        const hash = createHash("sha256");
        for await (const chunk of createReadStream(path)) hash.update(chunk);
        binaries.push({name, sizeBytes, sha256: hash.digest("hex")});
    }
    const manifest = {version, profile, timestamp: new Date().toISOString(), binaries};
    const checksums = binaries.map(({sha256, name}) => `${sha256}  ${name}\n`).join("");
    await writeFile(join(directory, "BUILD-MANIFEST.json"), JSON.stringify(manifest, null, 2) + "\n");
    await writeFile(join(directory, "SHA256SUMS.txt"), checksums);
' -- "$stage_dir" "$version" "$profile" "${binary_names[@]}"

if [[ -n "$archive_path" ]]; then
    [[ ! -d "$archive_path" && ! -L "$archive_path" ]] || fail "Cannot replace archive at $archive_path"
    tar -czf "$work_dir/$bundle_name.tar.gz" -C "$work_dir" "$bundle_name"
fi

previous_bundle="$work_dir/previous-bundle"
if [[ -e "$bundle_path" || -L "$bundle_path" ]]; then
    [[ -d "$bundle_path" && ! -L "$bundle_path" ]] || fail "Cannot replace bundle at $bundle_path"
    mv -- "$bundle_path" "$previous_bundle"
fi
if ! mv -- "$stage_dir" "$bundle_path"; then
    if [[ -d "$previous_bundle" ]]; then mv -- "$previous_bundle" "$bundle_path"; fi
    fail "Cannot publish bundle at $bundle_path"
fi
if [[ -n "$archive_path" ]] && ! mv -f -- "$work_dir/$bundle_name.tar.gz" "$archive_path"; then
    mv -- "$bundle_path" "$stage_dir"
    if [[ -d "$previous_bundle" ]]; then mv -- "$previous_bundle" "$bundle_path"; fi
    fail "Cannot publish archive at $archive_path"
fi

printf 'Bundle: %s\n' "$bundle_path"
if [[ -n "$archive_path" ]]; then
    printf 'Archive: %s\n' "$archive_path"
fi
