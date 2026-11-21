#!/usr/bin/env bash
set -euo pipefail

# Cross-platform build script (macOS/Linux first; Windows can still use build.ps1).
# Builds PowerShellEngineHost and optionally the cross-platform UI prototype.

ROOT="$(cd "$(dirname "$0")" && pwd)"
DIST="$ROOT/dist-crossplatform"
CONFIG="Release"
RID=""
SELF_CONTAINED="false"
SKIP_ENGINE="0"
SKIP_UI="0"

usage() {
  cat <<EOF
Usage: ./build.sh [options]
  --out-dir <dir>        Output directory (default: dist-crossplatform)
  --configuration <cfg>  Release/Debug (default: Release)
  --rid <rid>            Runtime identifier (e.g. osx-arm64, osx-x64, linux-x64)
  --self-contained       Publish self-contained engine
  --skip-engine          Skip dotnet engine publish
  --skip-ui              Skip cross-platform UI build
  -h, --help             Show help
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --out-dir) DIST="$2"; shift 2;;
    --configuration) CONFIG="$2"; shift 2;;
    --rid) RID="$2"; shift 2;;
    --self-contained) SELF_CONTAINED="true"; shift;;
    --skip-engine) SKIP_ENGINE="1"; shift;;
    --skip-ui) SKIP_UI="1"; shift;;
    -h|--help) usage; exit 0;;
    *) echo "Unknown arg: $1"; usage; exit 1;;
  esac
done

mkdir -p "$DIST"

if [[ "$SKIP_ENGINE" != "1" ]]; then
  if ! command -v dotnet >/dev/null 2>&1; then
    echo "dotnet not found; cannot build PowerShellEngineHost. Install .NET SDK 10+." >&2
    echo "Skipping engine build." >&2
  else
    echo "== Building PowerShellEngineHost ($CONFIG) =="
    PROJ="$ROOT/PowerShellEngineHost/PowerShellEngineHost/PowerShellEngineHost.csproj"
    ARGS=(publish "$PROJ" -c "$CONFIG" -o "$DIST" --nologo)
    if [[ -n "$RID" ]]; then ARGS+=(-r "$RID"); fi
    if [[ "$SELF_CONTAINED" == "true" ]]; then ARGS+=(--self-contained true); else ARGS+=(--self-contained false); fi
    dotnet "${ARGS[@]}"

    # Start-Job in the hosted PowerShell SDK spawns a pwsh process. In published layouts
    # it may look under runtimes/unix/lib/net10.0/pwsh; ensure a pwsh is discoverable.
    if [[ "$(uname -s)" != "MINGW"* && "$(uname -s)" != "MSYS"* && "$(uname -s)" != "CYGWIN"* ]]; then
      PWSH_BIN="$(command -v pwsh || true)"
      if [[ -n "$PWSH_BIN" ]]; then
        mkdir -p "$DIST/runtimes/unix/lib/net10.0"
        ln -sf "$PWSH_BIN" "$DIST/runtimes/unix/lib/net10.0/pwsh"
        echo "Linked pwsh for Start-Job: $PWSH_BIN"
      else
        echo "Warning: pwsh not found on PATH; Start-Job may fail in hosted engine." >&2
      fi
    fi
  fi
fi

if [[ "$SKIP_UI" != "1" ]]; then
  if ! command -v cargo >/dev/null 2>&1; then
    echo "cargo not found; cannot build cross-platform UI." >&2
  else
    echo "== Building cross-platform UI prototype =="
    (cd "$ROOT/cross-platform-ui" && cargo build --release)
    cp "$ROOT/cross-platform-ui/target/release/ps7ise-crossplatform-prototype" "$DIST/" 2>/dev/null || true
  fi
fi

echo "Done. Output: $DIST"
