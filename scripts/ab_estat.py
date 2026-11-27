"""Estatística do A/B dentro da execução (`SPARK_AB`): mantido para quem já o chama; o código está em
`scripts/motor/estat.py` e o comando novo é `python scripts/motor.py ruido|ab`.

Uso: python scripts/ab_estat.py <resumo.json ou pasta da sessão> [--gravar-ruido ruido.json]
"""

import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from motor.estat import (EQUIV_PCT, PARES_MIN, QUADROS_MIN, T_DECIDE, ajuste, analisar, blocos, pares,  # noqa: E402,F401
                         pct, ruido, t95)
from motor.sessao import ler_resumo  # noqa: E402,F401


def main() -> int:
    args = sys.argv[1:]
    if not args:
        print(__doc__)
        return 2
    ab = ler_resumo(args[0]).get("ab")
    if not ab:
        print("esta sessão não tem A/B (SPARK_AB)")
        return 1
    if "--gravar-ruido" in args:
        destino = Path(args[args.index("--gravar-ruido") + 1])
        r = ruido(ab)
        destino.write_text(json.dumps(r, indent=1), encoding="utf-8")
        print(json.dumps(r))
        return 0
    print(json.dumps(analisar(ab, teto=True), ensure_ascii=False))
    return 0


if __name__ == "__main__":
    sys.exit(main())
