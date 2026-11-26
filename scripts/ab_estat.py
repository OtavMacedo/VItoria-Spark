"""Estatística do A/B dentro da execução (`SPARK_AB`), sobre os blocos que o `resumo.json` traz em `ab.block_stats`.

Uso: python scripts/ab_estat.py <resumo.json ou pasta da sessão> [--ruido ruido.json] [--gravar-ruido ruido.json]

- Unidade = bloco (quadros vizinhos não são independentes). Por bloco, y = ln(mediana do quadro, ms) da cena estável.
- Modelo: y = a + b·índice_do_bloco + e·braço (mínimos quadrados; o termo linear tira a deriva lenta da cena; a ordem
  Thue-Morse dos blocos já a equilibra). e = efeito de B sobre A em escala log: exp(e) - 1 é a mudança do quadro
  (negativo = B mais rápido).
- Parada sequencial (pensada para olhar a cada bloco novo): com pelo menos `PARES_MIN` pares,
  |t| >= 3 decide (GANHO se B mais rápido, REGRESSAO se mais lento); IC 95 % inteiro dentro de ±`EQUIV_PCT` %
  decide SEM_EFEITO; senão continua. No teto: INCONCLUSIVO, com a menor diferença detectável (MDE ≈ 2,8·erro).
- `--gravar-ruido`: numa execução A/A (mesmo valor nos dois braços, ex. `SPARK_AB=VICTORY_STEP_INDEX=0/0`), grava o
  desvio dos resíduos por bloco e a MDE por nº de pares: o piso de ruído que o veredito usa.
"""

import json
import math
import sys
from pathlib import Path

PARES_MIN = 6
T_DECIDE = 3.0
EQUIV_PCT = 2.0
QUADROS_MIN = 5


def t95(gl: int) -> float:
    tabela = [12.706, 4.303, 3.182, 2.776, 2.571, 2.447, 2.365, 2.306, 2.262, 2.228, 2.201, 2.179, 2.160, 2.145,
              2.131, 2.120, 2.110, 2.101, 2.093, 2.086, 2.080, 2.074, 2.069, 2.064, 2.060, 2.056, 2.052, 2.048,
              2.045, 2.042]
    if gl <= 0:
        return math.inf
    return tabela[gl - 1] if gl <= 30 else 2.0 if gl <= 120 else 1.96


def ler_resumo(caminho: str | Path) -> dict:
    """O resumo.json de uma sessão (o arquivo, ou o mais novo numa pasta)."""
    p = Path(caminho)
    if p.is_dir():
        todos = sorted(p.rglob("resumo.json"), key=lambda q: q.stat().st_mtime)
        if not todos:
            raise FileNotFoundError(f"sem resumo.json em {p}")
        p = todos[-1]
    return json.loads(p.read_text(encoding="utf-8"))


def blocos(ab: dict) -> list[dict]:
    """Os blocos com quadros bastantes da cena estável."""
    return [b for b in (ab or {}).get("block_stats") or [] if b.get("n", 0) >= QUADROS_MIN and b.get("p50_ms")]


def _resolver(m: list[list[float]], v: list[float]) -> list[float]:
    """Resolve m·x = v (pequeno, por eliminação de Gauss)."""
    n = len(v)
    a = [row[:] + [v[i]] for i, row in enumerate(m)]
    for c in range(n):
        piv = max(range(c, n), key=lambda r: abs(a[r][c]))
        a[c], a[piv] = a[piv], a[c]
        if abs(a[c][c]) < 1e-12:
            raise ZeroDivisionError("sistema singular")
        for r in range(n):
            if r != c:
                f = a[r][c] / a[c][c]
                a[r] = [x - f * y for x, y in zip(a[r], a[c])]
    return [a[i][n] / a[i][i] for i in range(n)]


def ajuste(bs: list[dict], tendencia: bool = True, campo: str = "p50_ms") -> dict | None:
    """Mínimos quadrados de ln(campo) em [1, índice, braço]: efeito, erro, t, graus de liberdade, desvio dos
    resíduos."""
    bs = [b for b in bs if b.get(campo)]
    cols = 3 if tendencia else 2
    n = len(bs)
    if n < cols + 2 or len({b["arm"] for b in bs}) < 2:
        return None
    x = [[1.0] + ([float(b["i"])] if tendencia else []) + [float(b["arm"])] for b in bs]
    y = [math.log(b[campo]) for b in bs]
    xtx = [[sum(r[i] * r[j] for r in x) for j in range(cols)] for i in range(cols)]
    xty = [sum(r[i] * yy for r, yy in zip(x, y)) for i in range(cols)]
    try:
        beta = _resolver(xtx, xty)
        inv_ultima = _resolver(xtx, [0.0] * (cols - 1) + [1.0])[-1]
    except ZeroDivisionError:
        return None
    res = [yy - sum(b * c for b, c in zip(beta, r)) for r, yy in zip(x, y)]
    gl = n - cols
    s2 = sum(r * r for r in res) / gl
    erro = math.sqrt(max(s2 * inv_ultima, 0.0))
    e = beta[-1]
    return {"efeito_log": e, "erro_log": erro, "t": e / erro if erro > 0 else math.inf, "gl": gl,
            "sd_bloco": math.sqrt(s2), "blocos": n}


def pares(bs: list[dict]) -> list[float]:
    """d = ln(p50 B) - ln(p50 A) dos pares de blocos vizinhos (2j, 2j+1), um de cada braço."""
    por = {b["i"]: b for b in bs}
    out = []
    for i, b in por.items():
        o = por.get(i + 1)
        if i % 2 == 0 and o and o["arm"] != b["arm"]:
            a, bb = (b, o) if b["arm"] == 0 else (o, b)
            out.append(math.log(bb["p50_ms"]) - math.log(a["p50_ms"]))
    return out


def pct(x_log: float) -> float:
    return (math.exp(x_log) - 1) * 100


def analisar(ab: dict, teto: bool = False) -> dict:
    """A decisão do A/B agora. `teto`: a execução acabou (sem mais blocos): o que não decidiu vira INCONCLUSIVO."""
    bs = blocos(ab)
    d = pares(bs)
    fit = ajuste(bs)
    out = {"pares": len(d), "blocos": len(bs)}
    if fit is None or len(d) < 2:
        out.update(estado="INCONCLUSIVO" if teto else "CONTINUA", motivo="blocos insuficientes")
        return out
    meia = t95(fit["gl"]) * fit["erro_log"]
    out.update(
        efeito_pct=round(pct(fit["efeito_log"]), 2),
        ic95_pct=[round(pct(fit["efeito_log"] - meia), 2), round(pct(fit["efeito_log"] + meia), 2)],
        t=round(fit["t"], 2),
        sd_bloco_pct=round(fit["sd_bloco"] * 100, 2),
        mde_pct=round(2.8 * fit["erro_log"] * 100, 2),
        pares_b_mais_rapido=sum(x < 0 for x in d),
    )
    # O p95 por bloco no mesmo modelo: ruidoso (~60 quadros por bloco), por isso também por bloco e com t.
    fit95 = ajuste(bs, campo="p95_ms")
    if fit95:
        meia95 = t95(fit95["gl"]) * fit95["erro_log"]
        out.update(p95_efeito_pct=round(pct(fit95["efeito_log"]), 2),
                   p95_ic95_pct=[round(pct(fit95["efeito_log"] - meia95), 2),
                                 round(pct(fit95["efeito_log"] + meia95), 2)],
                   p95_t=round(fit95["t"], 2), sd_bloco_p95_pct=round(fit95["sd_bloco"] * 100, 2))
    if len(d) >= PARES_MIN and abs(fit["t"]) >= T_DECIDE:
        out["estado"] = "GANHO" if fit["efeito_log"] < 0 else "REGRESSAO"
    elif len(d) >= PARES_MIN and -EQUIV_PCT <= out["ic95_pct"][0] and out["ic95_pct"][1] <= EQUIV_PCT:
        out["estado"] = "SEM_EFEITO"
    else:
        out["estado"] = "INCONCLUSIVO" if teto else "CONTINUA"
    return out


def ruido(ab: dict) -> dict:
    """O piso de ruído de uma execução A/A: desvio dos resíduos por bloco e MDE esperada por nº de pares."""
    bs = blocos(ab)
    fit = ajuste(bs)
    if fit is None:
        raise ValueError("A/A sem blocos bastantes")
    sd = fit["sd_bloco"]
    fit95 = ajuste(bs, campo="p95_ms")
    # Erro do efeito com n pares (2n blocos, metade em cada braço): ~ sd·2/sqrt(2n).
    mde = {str(n): round(2.8 * sd * 2 / math.sqrt(2 * n) * 100, 2) for n in (4, 6, 8, 12, 16, 24)}
    return {"sd_bloco_pct": round(sd * 100, 2), "blocos": fit["blocos"], "efeito_aa_pct": round(pct(fit["efeito_log"]), 2),
            "t_aa": round(fit["t"], 2), "mde_pct_por_pares": mde,
            "sd_bloco_p95_pct": round(fit95["sd_bloco"] * 100, 2) if fit95 else None,
            "efeito_aa_p95_pct": round(pct(fit95["efeito_log"]), 2) if fit95 else None}


def main() -> int:
    args = sys.argv[1:]
    if not args:
        print(__doc__)
        return 2
    resumo = ler_resumo(args[0])
    ab = resumo.get("ab")
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
