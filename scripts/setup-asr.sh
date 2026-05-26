#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
VENV="$ROOT/.venv-asr"

# Cursor's AppImage prepends its own bin/ to PATH and sets APPDIR/APPIMAGE, which
# breaks venv interpreters (sys.path misses site-packages). Use a clean env.
clean_python_env() {
  env -i \
    HOME="${HOME:-/tmp}" \
    USER="${USER:-$(id -un)}" \
    PATH="/usr/local/bin:/usr/bin:/bin" \
    LANG="${LANG:-C.UTF-8}" \
    LC_ALL="${LC_ALL:-C.UTF-8}" \
    TMPDIR="${TMPDIR:-/tmp}" \
    "$@"
}

PYTHON="${PYTHON:-/usr/bin/python3}"
REAL_PYTHON="$(clean_python_env readlink -f "$PYTHON")"

if [[ ! -x "$REAL_PYTHON" ]]; then
  echo "Python not found at $PYTHON (resolved: $REAL_PYTHON)." >&2
  echo "Install Python 3 and PyTorch CUDA, e.g. pacman -S python python-pytorch-opt-cuda" >&2
  exit 1
fi

echo "Using Python: $REAL_PYTHON"
if clean_python_env "$REAL_PYTHON" -c "import torch; print(f'PyTorch {torch.__version__}, CUDA={torch.cuda.is_available()}')" 2>/dev/null; then
  :
else
  echo "Warning: PyTorch is not importable from $REAL_PYTHON." >&2
  echo "On Arch/CachyOS install python-pytorch-opt-cuda, then recreate the venv with --system-site-packages." >&2
fi

venv_python_ok() {
  [[ -x "$VENV/bin/python" ]] || return 1
  clean_python_env "$VENV/bin/python" -c "
import sys
assert sys.executable.startswith('${VENV}'), sys.executable
import qwen_asr  # noqa: F401
" 2>/dev/null
}

recreate_venv() {
  rm -rf "$VENV"
  clean_python_env "$REAL_PYTHON" -m venv --copies --system-site-packages "$VENV"
  if [[ -f "$VENV/bin/python" ]] && ! clean_python_env "$VENV/bin/python" -c "import sys; sys.exit(0)" 2>/dev/null; then
    echo "Repairing venv interpreter copies..."
    cp "$REAL_PYTHON" "$VENV/bin/python"
    cp "$REAL_PYTHON" "$VENV/bin/python3"
    PY_MINOR="$(clean_python_env "$REAL_PYTHON" -c 'import sys; print(f"{sys.version_info.major}.{sys.version_info.minor}")')"
    cp "$REAL_PYTHON" "$VENV/bin/python${PY_MINOR}"
  fi
  clean_python_env "$VENV/bin/python" -m ensurepip --upgrade
}

if [[ ! -x "$VENV/bin/python" ]] || ! venv_python_ok; then
  recreate_venv
fi

clean_python_env "$VENV/bin/pip" install --upgrade pip
clean_python_env "$VENV/bin/pip" install -r "$ROOT/requirements-asr.txt"

rm -f "$ROOT/scripts/__pycache__/qwen_asr".*.pyc 2>/dev/null || true

if ! clean_python_env "$VENV/bin/python" -c "import qwen_asr, torch; print(f'qwen-asr ok, CUDA={torch.cuda.is_available()}')"; then
  echo "qwen-asr import failed after install; recreating venv once more..." >&2
  recreate_venv
  clean_python_env "$VENV/bin/pip" install --upgrade pip
  clean_python_env "$VENV/bin/pip" install -r "$ROOT/requirements-asr.txt"
  clean_python_env "$VENV/bin/python" -c "import qwen_asr, torch; print(f'qwen-asr ok, CUDA={torch.cuda.is_available()}')"
fi

cat <<EOF

Qwen3 ASR (official PyTorch) is ready.

  Python:  $VENV/bin/python
  Models:  downloaded from Hugging Face on first use
           - Qwen/Qwen3-ASR-0.6B or Qwen/Qwen3-ASR-1.7B
           - Qwen/Qwen3-ForcedAligner-0.6B

GPU: uses PyTorch CUDA when available (python-pytorch-opt-cuda on Arch).
Set KPOPMVLYRICS_ASR_DEVICE=cpu to force CPU inference.

EOF
