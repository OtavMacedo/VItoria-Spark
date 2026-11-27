"""Veredito de um teste de desempenho: mantido para quem já o chama; o código está em `scripts/motor/veredito.py`
(os critérios estão no cabeçalho dele) e os comandos novos são `python scripts/motor.py ab|comparar|consultar`.

Uso:
  python scripts/veredito.py <sessão A/B> [--exp N]                         # = motor.py ab
  python scripts/veredito.py --a <sessão> [...] --b <sessão> [...]          # = motor.py comparar
  python scripts/veredito.py --consultar <decisoes.jsonl> [<NOME>]          # = motor.py consultar
  opções: [--imagem <json>] [--ruido <ruido.json>] [--registrar <decisoes.jsonl> --chave <NOME> [--sha <commit>]]
"""

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from motor.cli import main as motor  # noqa: E402


def main() -> int:
    args = sys.argv[1:]
    if not args:
        print(__doc__)
        return 2
    if args[0] == "--consultar":
        return motor(["consultar", *args[1:]])
    return motor(["comparar" if "--a" in args else "ab", *args])


if __name__ == "__main__":
    for saida in (sys.stdout, sys.stderr):
        saida.reconfigure(encoding="utf-8", errors="replace")
    sys.exit(main())
