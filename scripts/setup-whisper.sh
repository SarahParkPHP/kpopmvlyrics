#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
VENV="$ROOT/.venv-whisper"

PYTHON="${PYTHON:-/usr/bin/python3}"

if [[ ! -x "$PYTHON" ]]; then
  echo "Python not found at $PYTHON. Set PYTHON to your system python3." >&2
  exit 1
fi

if [[ ! -d "$VENV" ]]; then
  echo "Creating virtual environment at $VENV"
  "$PYTHON" -m venv --copies "$VENV"
fi

"$VENV/bin/pip" install --upgrade pip
"$VENV/bin/pip" install -r "$ROOT/requirements-whisper.txt"

echo
echo "Whisper is ready."
echo "The app will use: $VENV/bin/python"
