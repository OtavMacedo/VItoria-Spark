"""Estatística do motor: blocos de um A/B na execução, fatorial, entre execuções e intervalos.

- Unidade = bloco (quadros vizinhos não são independentes). Por bloco, y = ln(mediana do quadro, ms) da cena estável.
- A/B: y = a + b·índice + e·braço (mínimos quadrados; o termo linear tira a deriva lenta da cena, que a ordem
  Thue-Morse dos blocos já equilibra). exp(e) - 1 é a mudança do quadro (negativo = B mais rápido).
- Fatorial (`SPARK_AB=A*B*C`): y = a + b·índice + Σ e_f·nível_f. Cada bloco informa todos os fatores; um segundo
  ajuste com os produtos dois a dois acusa interação (|t| >= 3): aí os efeitos não se somam.
- Parada sequencial (para olhar a cada bloco novo): com `PARES_MIN` pares (ou `2·PARES_MIN` blocos por fator),
  |t| >= 3 decide (GANHO se B mais rápido, REGRESSAO se mais lento); IC 95 % dentro de ±`EQUIV_PCT` % decide
  SEM_EFEITO; senão continua. No teto: INCONCLUSIVO, com a menor diferença detectável (MDE ≈ 2,8·erro).
"""

import math

PARES_MIN = 6
T_DECIDE = 3.0
EQUIV_PCT = 2.0
QUADROS_MIN = 5


def t95(gl: int) -> float:
    """Ponto bilateral de 95 % do t de Student com `gl` graus de liberdade."""
    tabela = [12.706, 4.303, 3.182, 2.776, 2.571, 2.447, 2.365, 2.306, 2.262, 2.228, 2.201, 2.179, 2.160, 2.145,
              2.131, 2.120, 2.110, 2.101, 2.093, 2.086, 2.080, 2.074, 2.069, 2.064, 2.060, 2.056, 2.052, 2.048,
              2.045, 2.042]
    if gl <= 0:
        return math.inf
    return tabela[gl - 1] if gl <= 30 else 2.0 if gl <= 120 else 1.96


def pct(x_log: float) -> float:
    """Diferença em escala log como porcentagem."""
    return (math.exp(x_log) - 1) * 100


def intervalo_da_mediana(valores: list[float]) -> tuple[float, float, float]:
    """(baixo, mediana, alto): intervalo de 95 % da mediana pelas estatísticas de ordem (aproximação normal da
    binomial)."""
    s = sorted(valores)
    n = len(s)
    h = 1.96 * math.sqrt(n) / 2
    baixo = max(0, math.floor(n / 2 - h))
    alto = min(n - 1, math.ceil(n / 2 + h))
    return s[baixo], s[n // 2], s[alto]


def fisher_mais(a_longos: int, a_n: int, b_longos: int, b_n: int) -> float:
    """p unilateral (B tem mais quadros longos) do teste exato de Fisher."""
    total_longos = a_longos + b_longos
    n = a_n + b_n
    if total_longos == 0 or n == 0:
        return 1.0

    def hiper(k: int) -> float:
        return math.comb(b_n, k) * math.comb(a_n, total_longos - k) / math.comb(n, total_longos)

    return sum(hiper(k) for k in range(b_longos, min(b_n, total_longos) + 1))


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


def regressao(x: list[list[float]], y: list[float]) -> dict | None:
    """Mínimos quadrados de y em x: coeficientes, erro padrão de cada um, graus de liberdade e desvio dos resíduos.
    None sem dados bastantes ou com colunas dependentes."""
    n, cols = len(y), len(x[0]) if x else 0
    if n < cols + 2:
        return None
    xtx = [[sum(r[i] * r[j] for r in x) for j in range(cols)] for i in range(cols)]
    xty = [sum(r[i] * yy for r, yy in zip(x, y)) for i in range(cols)]
    try:
        beta = _resolver(xtx, xty)
        diag = [_resolver(xtx, [1.0 if j == i else 0.0 for j in range(cols)])[i] for i in range(cols)]
    except ZeroDivisionError:
        return None
    res = [yy - sum(b * c for b, c in zip(beta, r)) for r, yy in zip(x, y)]
    gl = n - cols
    s2 = sum(r * r for r in res) / gl
    return {"beta": beta, "erro": [math.sqrt(max(s2 * d, 0.0)) for d in diag], "gl": gl, "sd": math.sqrt(s2), "n": n}


def blocos(ab: dict) -> list[dict]:
    """Os blocos com quadros bastantes da cena estável."""
    return [b for b in (ab or {}).get("block_stats") or [] if b.get("n", 0) >= QUADROS_MIN and b.get("p50_ms")]


def ajuste(bs: list[dict], tendencia: bool = True, campo: str = "p50_ms") -> dict | None:
    """Mínimos quadrados de ln(campo) em [1, índice, braço]: efeito, erro, t, graus de liberdade, desvio dos
    resíduos."""
    bs = [b for b in bs if b.get(campo)]
    if len({b["arm"] for b in bs}) < 2:
        return None
    x = [[1.0] + ([float(b["i"])] if tendencia else []) + [float(b["arm"])] for b in bs]
    fit = regressao(x, [math.log(b[campo]) for b in bs])
    if fit is None:
        return None
    e, erro = fit["beta"][-1], fit["erro"][-1]
    return {"efeito_log": e, "erro_log": erro, "t": e / erro if erro > 0 else math.inf, "gl": fit["gl"],
            "sd_bloco": fit["sd"], "blocos": fit["n"]}


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


def _decidir(efeito_log: float, t: float, ic: list[float], bastante: bool, teto: bool) -> str:
    if bastante and abs(t) >= T_DECIDE:
        return "GANHO" if efeito_log < 0 else "REGRESSAO"
    if bastante and -EQUIV_PCT <= ic[0] and ic[1] <= EQUIV_PCT:
        return "SEM_EFEITO"
    return "INCONCLUSIVO" if teto else "CONTINUA"


def fatores(ab: dict) -> list[str]:
    """Os fatores de um A/B (`NOME=a/b`); um só num A/B simples (e em resumos antigos, sem o campo)."""
    return list((ab or {}).get("factors") or [])


def analisar(ab: dict, teto: bool = False) -> dict:
    """A decisão do A/B agora. `teto`: a execução acabou (sem mais blocos): o que não decidiu vira INCONCLUSIVO.
    Com mais de um fator, a de `analisar_fatorial`."""
    if len(fatores(ab)) > 1:
        return analisar_fatorial(ab, teto)
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
    out["estado"] = _decidir(fit["efeito_log"], fit["t"], out["ic95_pct"], len(d) >= PARES_MIN, teto)
    return out


def _niveis(b: dict, k: int) -> list[float]:
    celula = b.get("cell", b.get("arm", 0))
    return [float(celula >> f & 1) for f in range(k)]


def analisar_fatorial(ab: dict, teto: bool = False) -> dict:
    """A decisão de cada fator de um A/B fatorial, e as interações entre eles.

    Devolve `fatores` (um por fator: efeito, IC, t, estado, p95 e quadros longos por nível) e `interacoes`. O estado
    geral é DECIDIDO quando todos os fatores decidiram, CONTINUA enquanto falta algum e INCONCLUSIVO no teto.
    Um fator que interage com outro (|t| >= 3 no produto) não é GANHO sozinho: fica INCONCLUSIVO, com o par.
    """
    nomes = fatores(ab)
    k = len(nomes)
    bs = blocos(ab)
    out: dict = {"blocos": len(bs), "pares": len(bs) // 2, "fatores": [], "interacoes": []}
    x = [[1.0, float(b["i"])] + _niveis(b, k) for b in bs]
    fit = regressao(x, [math.log(b["p50_ms"]) for b in bs]) if bs else None
    if fit is None:
        out.update(estado="INCONCLUSIVO" if teto else "CONTINUA", motivo="blocos insuficientes")
        return out
    com95 = [b for b in bs if b.get("p95_ms")]
    fit95 = regressao([[1.0, float(b["i"])] + _niveis(b, k) for b in com95],
                      [math.log(b["p95_ms"]) for b in com95]) if com95 else None
    out.update(sd_bloco_pct=round(fit["sd"] * 100, 2))
    for f, nome in enumerate(nomes):
        e, erro = fit["beta"][2 + f], fit["erro"][2 + f]
        meia = t95(fit["gl"]) * erro
        por_nivel = [[b for b in bs if _niveis(b, k)[f] == nivel] for nivel in (0.0, 1.0)]
        ic = [round(pct(e - meia), 2), round(pct(e + meia), 2)]
        t = e / erro if erro > 0 else math.inf
        bastante = min(len(por_nivel[0]), len(por_nivel[1])) >= PARES_MIN
        fator = {
            "fator": nome, "efeito_pct": round(pct(e), 2), "ic95_pct": ic, "t": round(t, 2),
            "mde_pct": round(2.8 * erro * 100, 2), "blocos": [len(por_nivel[0]), len(por_nivel[1])],
            "longos": [sum(b.get("over_1s", 0) for b in por_nivel[0]), sum(b.get("n", 0) for b in por_nivel[0]),
                       sum(b.get("over_1s", 0) for b in por_nivel[1]), sum(b.get("n", 0) for b in por_nivel[1])],
            "estado": _decidir(e, t, ic, bastante, teto),
        }
        if fit95:
            e95, erro95 = fit95["beta"][2 + f], fit95["erro"][2 + f]
            fator.update(p95_efeito_pct=round(pct(e95), 2), p95_t=round(e95 / erro95 if erro95 > 0 else math.inf, 2))
        out["fatores"].append(fator)
    # Interações dois a dois: um segundo ajuste com os produtos.
    duplas = [(a, b) for a in range(k) for b in range(a + 1, k)]
    com = regressao([r + [r[2 + a] * r[2 + b] for a, b in duplas] for r in x],
                    [math.log(b["p50_ms"]) for b in bs]) if duplas else None
    if com:
        for j, (a, b) in enumerate(duplas):
            e, erro = com["beta"][2 + k + j], com["erro"][2 + k + j]
            t = e / erro if erro > 0 else math.inf
            if abs(t) >= T_DECIDE:
                out["interacoes"].append({"fatores": [nomes[a], nomes[b]], "efeito_pct": round(pct(e), 2),
                                          "t": round(t, 2)})
                for i in (a, b):
                    fator = out["fatores"][i]
                    fator.setdefault("interage_com", []).append(nomes[b if i == a else a])
                    if fator["estado"] == "GANHO":
                        fator["estado"] = "INCONCLUSIVO"
    estados = {f["estado"] for f in out["fatores"]}
    out["estado"] = "CONTINUA" if "CONTINUA" in estados else \
        "INCONCLUSIVO" if estados == {"INCONCLUSIVO"} else "DECIDIDO"
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
    return {"sd_bloco_pct": round(sd * 100, 2), "blocos": fit["blocos"],
            "efeito_aa_pct": round(pct(fit["efeito_log"]), 2), "t_aa": round(fit["t"], 2), "mde_pct_por_pares": mde,
            "sd_bloco_p95_pct": round(fit95["sd_bloco"] * 100, 2) if fit95 else None,
            "efeito_aa_p95_pct": round(pct(fit95["efeito_log"]), 2) if fit95 else None}


def entre_execucoes(a: list[float], b: list[float]) -> dict:
    """Comparação por execução (cada uma é uma unidade): ln do valor (mediana do quadro, ms), t de Welch."""
    ya, yb = [math.log(v) for v in a if v], [math.log(v) for v in b if v]
    out: dict = {"execucoes": [len(ya), len(yb)]}
    if len(ya) < 2 or len(yb) < 2:
        out["estado"] = "PROBLEMA_DE_MEDICAO"
        out["motivo"] = "são precisas 2+ execuções com cena estável de cada lado"
        return out
    ma, mb = sum(ya) / len(ya), sum(yb) / len(yb)
    va = sum((y - ma) ** 2 for y in ya) / (len(ya) - 1)
    vb = sum((y - mb) ** 2 for y in yb) / (len(yb) - 1)
    erro = math.sqrt(va / len(ya) + vb / len(yb)) or 1e-9
    gl = max(1, round((va / len(ya) + vb / len(yb)) ** 2 / (
        (va / len(ya)) ** 2 / max(len(ya) - 1, 1) + (vb / len(yb)) ** 2 / max(len(yb) - 1, 1) or 1e-18)))
    e = mb - ma
    meia = t95(gl) * erro
    out.update(efeito_pct=round(pct(e), 2), ic95_pct=[round(pct(e - meia), 2), round(pct(e + meia), 2)],
               t=round(e / erro, 2), mde_pct=round(2.8 * erro * 100, 2))
    if out["ic95_pct"][1] < 0:
        out["estado"] = "GANHO"
    elif out["ic95_pct"][0] > 0:
        out["estado"] = "REGRESSAO"
    elif -2 <= out["ic95_pct"][0] and out["ic95_pct"][1] <= 2:
        out["estado"] = "SEM_EFEITO"
    else:
        out["estado"] = "INCONCLUSIVO"
    return out
