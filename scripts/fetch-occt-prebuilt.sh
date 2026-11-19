#!/usr/bin/env bash
# Idempotently fetch prebuilt OpenCASCADE (OCCT 7.8+) dynamic libraries for macOS / Linux
# from conda-forge into cpp/occt-prebuilt/, strictly enforcing LGPL-2.1 dynamic linking.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
TARGET_DIR="${REPO_ROOT}/cpp/occt-prebuilt"
OCCT_VERSION="7.8"
DRY_RUN=0
FORCE=0

usage() {
  cat <<EOF
Usage: $(basename "$0") [OPTIONS]

Options:
  --dry-run            Inspect environment and print planned command & CMake guidance
  --version <ver>      Minimum OCCT version from conda-forge (default: 7.8)
  --prefix <dir>       Target installation prefix (default: cpp/occt-prebuilt)
  --force              Remove existing target directory and reinstall
  -h, --help           Show this help message
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --dry-run)
      DRY_RUN=1
      shift
      ;;
    --version)
      OCCT_VERSION="$2"
      shift 2
      ;;
    --prefix)
      TARGET_DIR="$2"
      shift 2
      ;;
    --force)
      FORCE=1
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "Unknown option: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

OS_NAME="$(uname -s)"
ARCH_NAME="$(uname -m)"
INCLUDE_DIR="${TARGET_DIR}/include/opencascade"
LIB_DIR="${TARGET_DIR}/lib"
VERSION_HEADER="${INCLUDE_DIR}/Standard_Version.hxx"

find_tool() {
  if command -v micromamba >/dev/null 2>&1; then
    echo "micromamba"
    return 0
  fi
  if command -v conda >/dev/null 2>&1; then
    echo "conda"
    return 0
  fi
  return 1
}

is_installed() {
  [[ -f "${VERSION_HEADER}" ]] && [[ -d "${LIB_DIR}" ]] && \
    (compgen -G "${LIB_DIR}/libTKernel*.so*" >/dev/null || compgen -G "${LIB_DIR}/libTKernel*.dylib" >/dev/null)
}

echo "=== AI-Native CAD: OCCT Prebuilt Fetcher (${OS_NAME}/${ARCH_NAME}) ==="
echo "Repo Root   : ${REPO_ROOT}"
echo "Target Dir  : ${TARGET_DIR}"
echo "Min Version : ${OCCT_VERSION} (conda-forge)"
echo "License Mode: LGPL-2.1 Dynamic Linking ONLY (.so/.dylib; no static OCCT linking)"

if is_installed && [[ "${FORCE}" -eq 0 ]]; then
  echo "[OK] Prebuilt OCCT already present at: ${TARGET_DIR}"
  echo "     Headers : ${INCLUDE_DIR}"
  echo "     Libs    : ${LIB_DIR}"
  echo "     Pass --force to reinstall."
  exit 0
fi

TOOL=""
if TOOL="$(find_tool)"; then
  echo "[INFO] Detected package manager: ${TOOL} ($(command -v "${TOOL}"))"
else
  echo "[WARN] Neither 'micromamba' nor 'conda' was found in PATH."
  echo "       Install micromamba: curl -Ls https://micro.mamba.pm/install.sh | bash"
  TOOL=""
fi

PLANNED_TOOL="${TOOL:-micromamba}"
PKG_SPEC="occt>=${OCCT_VERSION}"

if [[ "${DRY_RUN}" -eq 1 ]]; then
  echo ""
  echo "[DRY-RUN] Environment check complete. No files were downloaded or modified."
  echo "[DRY-RUN] Planned command:"
  echo "  ${PLANNED_TOOL} create -y -p \"${TARGET_DIR}\" -c conda-forge \"${PKG_SPEC}\""
  echo ""
  echo "[DRY-RUN] Expected CMake / dynamic linker settings:"
  echo "  -DCMAKE_PREFIX_PATH=\"${TARGET_DIR}\""
  echo "  -DOpenCASCADE_DIR=\"${TARGET_DIR}/lib/cmake/opencascade\""
  if [[ "${OS_NAME}" == "Darwin" ]]; then
    echo "  export DYLD_LIBRARY_PATH=\"${LIB_DIR}:\${DYLD_LIBRARY_PATH:-}\""
  else
    echo "  export LD_LIBRARY_PATH=\"${LIB_DIR}:\${LD_LIBRARY_PATH:-}\""
  fi
  exit 0
fi

if [[ -z "${TOOL}" ]]; then
  echo "[ERROR] Cannot fetch OCCT because neither 'micromamba' nor 'conda' is installed." >&2
  exit 1
fi

if [[ "${FORCE}" -eq 1 && -d "${TARGET_DIR}" ]]; then
  echo "[INFO] --force specified; removing ${TARGET_DIR} ..."
  rm -rf "${TARGET_DIR}"
fi

mkdir -p "$(dirname "${TARGET_DIR}")"
echo "[RUN] ${TOOL} create -y -p \"${TARGET_DIR}\" -c conda-forge \"${PKG_SPEC}\""
"${TOOL}" create -y -p "${TARGET_DIR}" -c conda-forge "${PKG_SPEC}"

if ! is_installed; then
  echo "[ERROR] OCCT files not found under ${TARGET_DIR} after installation." >&2
  exit 1
fi

cat > "${TARGET_DIR}/.occt-prebuilt-manifest.json" <<EOF
{
  "platform": "${OS_NAME}-${ARCH_NAME}",
  "package": "${PKG_SPEC}",
  "installer": "${TOOL}",
  "license": "LGPL-2.1-only",
  "linking": "dynamic",
  "include_dir": "${INCLUDE_DIR}",
  "lib_dir": "${LIB_DIR}"
}
EOF

echo "[OK] OCCT prebuilt installed successfully at ${TARGET_DIR}."
