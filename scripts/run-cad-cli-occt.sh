#!/usr/bin/env bash
# Bootstrap OCCT 7.9+ runtime libraries, then exec target/occt-ffi/(release|debug)/cad-cli.
# Every argument is forwarded unchanged. A missing prebuilt OCCT, bridge, or
# system runtime library is a hard error: this script never launches a non-OCCT cad-cli.
#
# Hermetic checks (not cad-cli arguments):
#   AICAD_OCCT_BOOTSTRAP_ROOT    checkout to probe instead of this repo
#   AICAD_OCCT_BOOTSTRAP_DRY_RUN 1 = print the resolved argv and library dirs, do not exec

set -euo pipefail

missing=()
fixes=()
library_dirs=()

quote_path() {
    printf '"%s"' "$1"
}

is_dynamic_lib() {
    local path=$1
    local kind=$2
    [[ -f $path ]] || return 1
    local magic
    magic=$(od -An -N 4 -t x1 "$path" 2>/dev/null | tr -d ' \n\r' | tr '[:upper:]' '[:lower:]' || true)
    [[ -n $magic ]] || return 1
    case "$kind" in
        pe) [[ $magic == 4d5a* ]] ;;
        macho)
            [[ $magic == feedface || $magic == feedfacf || $magic == cefaedfe || $magic == cffaedfe || $magic == cafebabe ]]
            ;;
        elf) [[ $magic == 7f454c46 ]] ;;
        *) return 1 ;;
    esac
}

library_present() {
    local dir=$1
    local canonical=$2
    local allow_versioned=$3
    local kind=$4
    [[ -d $dir ]] || return 1
    if is_dynamic_lib "$dir/$canonical" "$kind"; then
        return 0
    fi
    [[ $allow_versioned == yes ]] || return 1
    local stem=${canonical%.*}
    local nullglob_was=0
    shopt -q nullglob && nullglob_was=1
    shopt -s nullglob
    local entry
    local found=1
    for entry in "$dir/$stem".* "$dir/$stem"-*; do
        if [[ -f $entry ]] && is_dynamic_lib "$entry" "$kind"; then
            found=0
            break
        fi
    done
    if ((nullglob_was == 0)); then
        shopt -u nullglob
    fi
    return "$found"
}

add_library_dir() {
    local dir=$1
    [[ -n $dir && -d $dir ]] || return 0
    local full
    full=$(cd -- "$dir" && pwd -P)
    local existing
    for existing in "${library_dirs[@]+"${library_dirs[@]}"}"; do
        if [[ $existing == "$full" ]]; then
            return 0
        fi
    done
    library_dirs+=("$full")
}

extract_define() {
    awk -v name="$2" '$1 == "#define" && $2 == name && $3 ~ /^[0-9]+$/ { print $3; exit }' "$1"
}

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)
if [[ -n ${AICAD_OCCT_BOOTSTRAP_ROOT:-} ]]; then
    bootstrap_override=1
    repo_root=$(cd -- "$AICAD_OCCT_BOOTSTRAP_ROOT" && pwd -P)
else
    bootstrap_override=0
    repo_root=$(cd -- "$script_dir/.." && pwd -P)
fi

os_name=$(uname -s)
case "$os_name" in
    Linux)
        exe_suffix=
        bridge_name=libocct_bridge.so
        lib_prefix=lib
        lib_suffix=so
        runtime_rel=cpp/occt-prebuilt/lib
        path_var=LD_LIBRARY_PATH
        path_sep=:
        lib_kind=elf
        crt_names=(libstdc++.so.6)
        crt_fix="sudo apt-get install --yes libstdc++6"
        crt_dirs=(/lib /lib64 /usr/lib /usr/lib64 /lib/x86_64-linux-gnu /usr/lib/x86_64-linux-gnu /lib/aarch64-linux-gnu /usr/lib/aarch64-linux-gnu)
        ;;
    Darwin)
        exe_suffix=
        bridge_name=libocct_bridge.dylib
        lib_prefix=lib
        lib_suffix=dylib
        runtime_rel=cpp/occt-prebuilt/lib
        path_var=DYLD_LIBRARY_PATH
        path_sep=:
        lib_kind=macho
        crt_names=(libc++.1.dylib)
        crt_fix="xcode-select --install"
        crt_dirs=(/usr/lib)
        ;;
    MINGW*|MSYS*|CYGWIN*)
        exe_suffix=.exe
        bridge_name=occt_bridge.dll
        lib_prefix=
        lib_suffix=dll
        runtime_rel=cpp/occt-prebuilt/Library/bin
        path_var=PATH
        path_sep=:
        lib_kind=pe
        crt_names=(vcruntime140.dll vcruntime140_1.dll msvcp140.dll)
        crt_fix="winget install --id Microsoft.VCRedist.2015+.x64 -e"
        crt_dirs=()
        if [[ -n ${SYSTEMROOT:-} && -d $SYSTEMROOT/System32 ]]; then
            crt_dirs+=("$SYSTEMROOT/System32")
        elif [[ -n ${WINDIR:-} && -d $WINDIR/System32 ]]; then
            crt_dirs+=("$WINDIR/System32")
        fi
        ;;
    *)
        printf 'Error: unsupported platform: %s\n' "$os_name" >&2
        exit 1
        ;;
esac

toolkit_stems=(
    TKernel TKMath TKBRep TKGeomBase TKG2d TKG3d TKGeomAlgo TKTopAlgo
    TKPrim TKBO TKFillet TKOffset TKShHealing TKMesh TKHLR
    TKCDF TKLCAF TKXCAF TKXSBase TKDESTEP TKDEIGES
)

if ((bootstrap_override == 0)) && [[ -n ${OCCT_BRIDGE_LIB_DIR:-} ]]; then
    add_library_dir "$OCCT_BRIDGE_LIB_DIR"
fi
for config in Release RelWithDebInfo MinSizeRel Debug; do
    add_library_dir "$repo_root/build/occt-bridge/$config"
done
add_library_dir "$repo_root/build/occt-bridge"
prebuilt_bin="$repo_root/$runtime_rel"
add_library_dir "$prebuilt_bin"
if [[ $lib_kind == pe ]]; then
    # Conda-forge packages on Windows place transitive runtime DLLs (e.g. zlib.dll) in the prefix root.
    add_library_dir "$repo_root/cpp/occt-prebuilt"
fi

bridge_found=0
for dir in "${library_dirs[@]+"${library_dirs[@]}"}"; do
    if library_present "$dir" "$bridge_name" yes "$lib_kind"; then
        bridge_found=1
        break
    fi
done
if ((bridge_found == 0)); then
    missing+=("$bridge_name")
    fixes+=("cmake -S $(quote_path "$repo_root/cpp/occt-bridge") -B $(quote_path "$repo_root/build/occt-bridge") && cmake --build $(quote_path "$repo_root/build/occt-bridge") --config Release")
fi

occt_missing=0
for stem in "${toolkit_stems[@]}"; do
    library="${lib_prefix}${stem}.${lib_suffix}"
    if ! library_present "$prebuilt_bin" "$library" yes "$lib_kind"; then
        occt_missing=1
        missing+=("$library")
    fi
done

for header in \
    "$repo_root/cpp/occt-prebuilt/Library/include/opencascade/Standard_Version.hxx" \
    "$repo_root/cpp/occt-prebuilt/include/opencascade/Standard_Version.hxx"
do
    [[ -f $header ]] || continue
    major=$(extract_define "$header" OCC_VERSION_MAJOR || true)
    minor=$(extract_define "$header" OCC_VERSION_MINOR || true)
    if [[ -z $major || -z $minor ]]; then
        continue
    fi
    if ((major < 7 || (major == 7 && minor < 9))); then
        occt_missing=1
        missing+=("OCCT>=7.9 (found ${major}.${minor} in Standard_Version.hxx)")
    fi
    break
done
if ((occt_missing == 1)); then
    if [[ $lib_kind == pe ]]; then
        fixes+=("powershell -ExecutionPolicy Bypass -File $(quote_path "$repo_root/scripts/fetch-occt-prebuilt.ps1")")
    else
        fixes+=("$(quote_path "$repo_root/scripts/fetch-occt-prebuilt.sh")")
    fi
fi

if [[ $lib_kind == elf && -n ${LD_LIBRARY_PATH:-} ]]; then
    IFS=':' read -r -a ld_entries <<<"$LD_LIBRARY_PATH"
    crt_dirs+=("${ld_entries[@]}")
elif [[ $lib_kind == macho && -n ${DYLD_LIBRARY_PATH:-} ]]; then
    IFS=':' read -r -a dyld_entries <<<"$DYLD_LIBRARY_PATH"
    crt_dirs+=("${dyld_entries[@]}")
elif [[ $lib_kind == pe && -n ${PATH:-} ]]; then
    IFS=':' read -r -a path_entries <<<"$PATH"
    crt_dirs+=("${path_entries[@]}")
fi
for dir in "${library_dirs[@]+"${library_dirs[@]}"}"; do
    crt_dirs+=("$dir")
done

crt_missing=0
for crt in "${crt_names[@]}"; do
    found=0
    for dir in "${crt_dirs[@]+"${crt_dirs[@]}"}"; do
        if library_present "$dir" "$crt" no "$lib_kind"; then
            found=1
            break
        fi
    done
    if ((found == 0)); then
        crt_missing=1
        missing+=("$crt")
    fi
done
if ((crt_missing == 1)); then
    fixes+=("$crt_fix")
fi

cad_cli=
search_cad_cli() {
    local base=$1
    local profile=$2
    local candidate="$base/occt-ffi/$profile/cad-cli$exe_suffix"
    if [[ -f $candidate ]]; then
        cad_cli=$(cd -- "$(dirname -- "$candidate")" && pwd -P)/$(basename -- "$candidate")
        return 0
    fi
    return 1
}
search_cad_cli "$repo_root/target" release || true
if [[ -z $cad_cli && $bootstrap_override -eq 0 && -n ${CARGO_TARGET_DIR:-} ]]; then
    search_cad_cli "$CARGO_TARGET_DIR" release || true
fi
if [[ -z $cad_cli ]]; then
    search_cad_cli "$repo_root/target" debug || true
fi
if [[ -z $cad_cli && $bootstrap_override -eq 0 && -n ${CARGO_TARGET_DIR:-} ]]; then
    search_cad_cli "$CARGO_TARGET_DIR" debug || true
fi
if [[ -z $cad_cli ]]; then
    missing+=("target/occt-ffi/release/cad-cli$exe_suffix")
    missing+=("target/occt-ffi/debug/cad-cli$exe_suffix")
    fixes+=("cargo build -p cad-cli --features occt-ffi --target-dir $(quote_path "$repo_root/target/occt-ffi") --locked")
fi

if ((${#missing[@]} > 0)); then
    printf '%s\n' "Error: OCCT runtime is incomplete. Refusing to launch a non-authoritative cad-cli." >&2
    for item in "${missing[@]}"; do
        printf 'Missing: %s\n' "$item" >&2
    done
    for fix in "${fixes[@]}"; do
        printf 'Fix: %s\n' "$fix" >&2
    done
    exit 1
fi

joined=
for dir in "${library_dirs[@]}"; do
    if [[ -z $joined ]]; then
        joined=$dir
    else
        joined="${joined}${path_sep}${dir}"
    fi
done
if [[ -n $joined ]]; then
    case "$path_var" in
        PATH) current=${PATH-} ;;
        LD_LIBRARY_PATH) current=${LD_LIBRARY_PATH-} ;;
        DYLD_LIBRARY_PATH) current=${DYLD_LIBRARY_PATH-} ;;
        *) current= ;;
    esac
    if [[ -n $current ]]; then
        export "$path_var=${joined}${path_sep}${current}"
    else
        export "$path_var=$joined"
    fi
    if [[ $lib_kind == macho ]]; then
        fallback=${DYLD_FALLBACK_LIBRARY_PATH-}
        if [[ -n $fallback ]]; then
            export "DYLD_FALLBACK_LIBRARY_PATH=${joined}${path_sep}${fallback}"
        else
            export "DYLD_FALLBACK_LIBRARY_PATH=$joined"
        fi
    fi
fi

if [[ ${AICAD_OCCT_BOOTSTRAP_DRY_RUN:-} == 1 ]]; then
    printf 'executable=%s\n' "$cad_cli"
    printf 'argv_count=%s\n' "$#"
    for arg in "$@"; do
        printf 'argv=%s\n' "$arg"
    done
    for dir in "${library_dirs[@]}"; do
        printf 'library_dir=%s\n' "$dir"
    done
    exit 0
fi

exec "$cad_cli" "$@"
