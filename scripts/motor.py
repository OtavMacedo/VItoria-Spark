"""Entrada do motor de diagnóstico: `python scripts/motor.py <comando> ...` (os comandos estão em `motor/cli.py`)."""

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from motor.cli import main  # noqa: E402

if __name__ == "__main__":
    for saida in (sys.stdout, sys.stderr):
        saida.reconfigure(encoding="utf-8", errors="replace")
    sys.exit(main())
