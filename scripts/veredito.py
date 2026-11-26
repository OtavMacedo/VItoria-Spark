"""Veredito de um teste de desempenho: 20 linhas fixas (`veredito.md`) e `veredito.json`, com um estado só.

Uso:
  python scripts/veredito.py <sessão A/B> [--exp N]                       # A/B na execução (SPARK_AB; N de SPARK_PLAN)
  python scripts/veredito.py --a <sessão> [<sessão> ...] --b <sessão> [...] # entre execuções (2+ de cada lado)
  opções: [--imagem <json>] [--ruido <ruido.json>] [--registrar <decisoes.jsonl> --chave <NOME> [--sha <commit>]]
  python scripts/veredito.py --consultar <decisoes.jsonl> [<NOME>]         # o que já foi decidido (antes de testar)

<sessão> = pasta de uma execução (a mais nova `resumo.json` dentro dela) ou o próprio `resumo.json`.

Estados: GANHO, SEM_EFEITO, INCONCLUSIVO, REGRESSAO, PROBLEMA_DE_MEDICAO. GANHO exige TODOS:
1. quadro (mediana) de B >= 3 % mais curto, com o intervalo de 95 % todo abaixo de 0 (A/B: |t| >= 3 com 6+ pares;
   ver `scripts/ab_estat.py`);
2. p95 de B não piora mais que 5 % com confiança (no A/B, o p95 de cada bloco no mesmo modelo, |t| >= 3: o p95 de
   uma execução A/A variou 23 % entre braços iguais, então um limiar sem estatística reprovaria por acaso);
3. quadros > 1 s não aumentam (teste exato de Fisher, unilateral, 5 %);
4. nenhuma falha nem perda de dispositivo em B;
5. imagem: B não perde detalhe que A tem nem ganha artefato (se `--imagem` for dado; o JSON vem de um medidor local
   por braço/execução, com `malha` e `artefato?` como frações de capturas: nada do título entra no repositório).
Uma piora em 2-5 é REGRESSAO mesmo com o quadro melhor. RAM livre mínima abaixo de 300 MB é só alerta (as duas
execuções/braços dividem a máquina). PROBLEMA_DE_MEDICAO: sem cena estável, sem blocos/execuções bastantes, ou ruído
acima de 2x o piso de `--ruido`: nesse caso (e só nele) vale abrir o log.
`--registrar` acrescenta uma linha a `decisoes.jsonl` (memória entre sessões; consulte antes de testar).
"""

import json
import math
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from ab_estat import analisar, blocos, ler_resumo, pct, t95  # noqa: E402

GANHO_MIN_PCT = 3.0
# Chaves cujo efeito na memória persiste depois que o braço troca (uma escrita perdida ou adiada fica velha até o
# fim da execução): no A/B na execução o braço A herda o estado do B, então a imagem por braço não vale; julgue a
# imagem com execuções separadas.
IMAGEM_SO_ENTRE_EXECUCOES = ("VICTORY_SKIP_WC_BACK", "VICTORY_MERGE_MASK")
P95_MAX_PCT = 5.0
RAM_ALERTA_MB = 300


def fisher_mais(a_longos: int, a_n: int, b_longos: int, b_n: int) -> float:
    """p unilateral (B tem mais quadros longos) do teste exato de Fisher."""
    total_longos = a_longos + b_longos
    n = a_n + b_n
    if total_longos == 0 or n == 0:
        return 1.0

    def hiper(k: int) -> float:
        return math.comb(b_n, k) * math.comb(a_n, total_longos - k) / math.comb(n, total_longos)

    return sum(hiper(k) for k in range(b_longos, min(b_n, total_longos) + 1))


def falhas(resumo: dict) -> list[str]:
    out = []
    if resumo.get("crash"):
        out.append("falha")
    if resumo.get("device_lost"):
        out.append("dispositivo perdido")
    return out


def entre_execucoes(a: list[dict], b: list[dict]) -> dict:
    """Comparação por execução (cada execução é uma unidade): ln da mediana da cena estável, t de Welch."""
    def p50(r):
        est = (r.get("stable_scene") or {}).get("frames") or {}
        return est.get("p50_ms") or (r.get("frames_world") or {}).get("p50_ms")
    ya = [math.log(v) for r in a if (v := p50(r))]
    yb = [math.log(v) for r in b if (v := p50(r))]
    out = {"execucoes": [len(ya), len(yb)]}
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


def veredito(sessoes_a: list[dict], sessoes_b: list[dict] | None, imagem: dict | None, ruido: dict | None) -> dict:
    v: dict = {"criterios": {}, "alertas": [], "modo": "ab" if sessoes_b is None else "execucoes"}
    if sessoes_b is None:
        resumo = sessoes_a[0]
        ab = resumo.get("ab")
        if not ab:
            return {**v, "estado": "PROBLEMA_DE_MEDICAO", "motivo": "a sessão não tem A/B (SPARK_AB)"}
        est = analisar(ab, teto=True)
        v["efeito"] = est
        v["planos"] = [(ab.get("a") or {}).get("sets"), (ab.get("b") or {}).get("sets")]
        a, b = ab.get("a") or {}, ab.get("b") or {}
        p95 = (a.get("p95_ms"), b.get("p95_ms"))
        longos = (a.get("long_frames_over_1s", 0), a.get("frames", 0), b.get("long_frames_over_1s", 0),
                  b.get("frames", 0))
        falhou_b = falhas(resumo)
        ram = [((resumo.get("memory") or {}).get("ram_free_min_mb"))]
        if not (resumo.get("stable_scene") or {}).get("from_s"):
            return {**v, "estado": "PROBLEMA_DE_MEDICAO", "motivo": "sem cena estável"}
        if ruido and est.get("sd_bloco_pct") and est["sd_bloco_pct"] > 2 * ruido.get("sd_bloco_pct", math.inf):
            return {**v, "estado": "PROBLEMA_DE_MEDICAO",
                    "motivo": f"ruído por bloco {est['sd_bloco_pct']} % > 2x o piso ({ruido['sd_bloco_pct']} %)"}
    else:
        est = entre_execucoes(sessoes_a, sessoes_b)
        v["efeito"] = est

        def soma(rs, k):
            return sum(((r.get("stable_scene") or {}).get("frames") or r.get("frames_world") or {}).get(k) or 0
                       for r in rs)

        def mediana_p95(rs):
            vals = sorted(((r.get("stable_scene") or {}).get("frames") or r.get("frames_world") or {}).get("p95_ms")
                          or 0 for r in rs)
            return vals[len(vals) // 2] if vals else None

        p95 = (mediana_p95(sessoes_a), mediana_p95(sessoes_b))
        longos = (sum(r.get("long_world_frames_over_1s", 0) for r in sessoes_a), soma(sessoes_a, "frames"),
                  sum(r.get("long_world_frames_over_1s", 0) for r in sessoes_b), soma(sessoes_b, "frames"))
        falhou_b = [f for r in sessoes_b for f in falhas(r)]
        ram = [(r.get("memory") or {}).get("ram_free_min_mb") for r in sessoes_a + sessoes_b]

    c = v["criterios"]
    e = est.get("efeito_pct")
    c["quadro"] = e is not None and e <= -GANHO_MIN_PCT and est.get("estado") == "GANHO"
    if "p95_t" in est:
        # A/B na execução: o p95 por bloco no mesmo modelo; só piora com confiança (|t| >= 3) e acima do limite.
        c["p95"] = not (est["p95_t"] >= 3 and est["p95_efeito_pct"] > P95_MAX_PCT)
    else:
        c["p95"] = not (p95[0] and p95[1] and (p95[1] / p95[0] - 1) * 100 > P95_MAX_PCT)
    p_longos = fisher_mais(*longos)
    c["longos"] = p_longos >= 0.05
    c["falhas"] = not falhou_b
    c["imagem"] = True
    persistente = [k for k in IMAGEM_SO_ENTRE_EXECUCOES if any(k in (pl or "") for pl in v.get("planos") or [])]
    if imagem and persistente:
        v["alertas"].append(f"imagem por braço não vale com {', '.join(persistente)} (o estado persiste entre "
                            "braços): julgue a imagem com execuções separadas")
        imagem = None
    if imagem:
        ia, ib = imagem.get("a") or {}, imagem.get("b") or {}
        perde = ia.get("malha", 0) - ib.get("malha", 0) > 0.5
        artefato = ib.get("artefato?", 0) - ia.get("artefato?", 0) > 0.5
        c["imagem"] = not (perde or artefato)
        v["imagem"] = {"a": ia, "b": ib}
    if any(r is not None and r < RAM_ALERTA_MB for r in ram):
        v["alertas"].append(f"RAM livre mínima {min(r for r in ram if r is not None)} MB")
    v["numeros"] = {"p95_ms": p95, "longos": longos, "p_longos": round(p_longos, 3), "falhas_b": falhou_b}

    estado = est.get("estado", "INCONCLUSIVO")
    guardas = all(c[k] for k in ("p95", "longos", "falhas", "imagem"))
    if est.get("estado") == "PROBLEMA_DE_MEDICAO":
        v["estado"] = estado
        v["motivo"] = est.get("motivo")
    elif not guardas:
        v["estado"] = "REGRESSAO"
        v["motivo"] = "falhou: " + ", ".join(k for k in ("p95", "longos", "falhas", "imagem") if not c[k])
    elif estado == "GANHO" and not c["quadro"]:
        v["estado"] = "SEM_EFEITO"
        v["motivo"] = f"ganho abaixo de {GANHO_MIN_PCT} %"
    elif estado == "CONTINUA":
        v["estado"] = "INCONCLUSIVO"
    else:
        v["estado"] = estado
    return v


def md(v: dict) -> str:
    est = v.get("efeito") or {}
    linhas = [f"# Veredito: {v['estado']}", ""]
    if v.get("motivo"):
        linhas.append(f"- motivo: {v['motivo']}")
    if v.get("planos"):
        linhas.append(f"- A: `{v['planos'][0]}` | B: `{v['planos'][1]}`")
    if "efeito_pct" in est:
        unid = f"{est.get('pares', 0)} pares de blocos" if v["modo"] == "ab" else \
            f"{est['execucoes'][0]}+{est['execucoes'][1]} execuções"
        linhas.append(f"- quadro (mediana), B contra A: {est['efeito_pct']:+.1f} % (IC 95 % {est['ic95_pct'][0]:+.1f} a "
                      f"{est['ic95_pct'][1]:+.1f}), t {est.get('t')}, {unid}")
        linhas.append(f"- menor diferença detectável: {est.get('mde_pct')} %"
                      + (f"; ruído por bloco {est['sd_bloco_pct']} %" if "sd_bloco_pct" in est else ""))
    n = v.get("numeros") or {}
    if n:
        p95 = n["p95_ms"]
        linhas.append(f"- p95: A {p95[0]} ms, B {p95[1]} ms"
                      + (f"; por bloco {est['p95_efeito_pct']:+.1f} % (IC {est['p95_ic95_pct'][0]:+.1f} a "
                         f"{est['p95_ic95_pct'][1]:+.1f}, t {est['p95_t']})" if "p95_t" in est else ""))
        lo = n["longos"]
        linhas.append(f"- quadros > 1 s: A {lo[0]} de {lo[1]}, B {lo[2]} de {lo[3]} (Fisher p = {n['p_longos']})")
        linhas.append(f"- falhas em B: {', '.join(n['falhas_b']) or 'nenhuma'}")
    if v.get("imagem"):
        ia, ib = v["imagem"]["a"], v["imagem"]["b"]
        linhas.append(f"- imagem: detalhe/malha/artefato A {ia.get('detalhe')}/{ia.get('malha')}/{ia.get('artefato?')}"
                      f", B {ib.get('detalhe')}/{ib.get('malha')}/{ib.get('artefato?')}")
    reprovou = [k for k, ok in (v.get("criterios") or {}).items() if not ok and k != "quadro"]
    linhas.append(f"- critérios que reprovaram: {', '.join(reprovou) or 'nenhum'}")
    for a in v.get("alertas", []):
        linhas.append(f"- alerta: {a}")
    acao = {
        "GANHO": "confirmar numa 2ª sessão independente (outra hora, outra ordem); com 2 GANHO, mesclar desligada e "
                 "propor ligar no perfil com a medição",
        "SEM_EFEITO": "registrar e arquivar",
        "INCONCLUSIVO": f"mais pares/execuções só se o ganho esperado passar de {est.get('mde_pct', '?')} %",
        "REGRESSAO": "rejeitar (ou corrigir o critério que reprovou e testar de novo)",
        "PROBLEMA_DE_MEDICAO": "ver o log (só neste caso) e repetir",
    }[v["estado"]]
    linhas.append(f"- próxima ação: {acao}")
    return "\n".join(linhas[:20]) + "\n"


def consultar(caminho: Path, nome: str | None) -> int:
    if not caminho.exists():
        print("nenhuma decisão registrada")
        return 0
    for linha in caminho.read_text(encoding="utf-8").splitlines():
        d = json.loads(linha)
        if nome is None or nome in d.get("chave", ""):
            print(f"{d['data']} {d['chave']} {d.get('sha', '')[:8]} {d['estado']} {d.get('efeito_pct')} % "
                  f"{d.get('ic95_pct')} {d.get('unidades')}")
    return 0


def main() -> int:
    args = sys.argv[1:]
    if not args:
        print(__doc__)
        return 2
    if args[0] == "--consultar":
        return consultar(Path(args[1]), args[2] if len(args) > 2 else None)

    def opcao(nome):
        if nome in args:
            i = args.index(nome)
            valor = args[i + 1]
            del args[i:i + 2]
            return valor
        return None

    exp = opcao("--exp")
    imagem = opcao("--imagem")
    ruido = opcao("--ruido")
    registrar = opcao("--registrar")
    chave = opcao("--chave")
    sha = opcao("--sha")
    if "--a" in args:
        ia, ib = args.index("--a"), args.index("--b")
        lado_a = args[ia + 1:ib] if ia < ib else args[ia + 1:]
        lado_b = args[ib + 1:] if ib > ia else args[ib + 1:ia]
        v = veredito([ler_resumo(s) for s in lado_a], [ler_resumo(s) for s in lado_b],
                     json.loads(Path(imagem).read_text(encoding="utf-8")) if imagem else None,
                     json.loads(Path(ruido).read_text(encoding="utf-8")) if ruido else None)
        destino = Path(lado_b[0])
    else:
        resumo = ler_resumo(args[0])
        if exp is not None:
            # Um experimento de uma execução com SPARK_PLAN.
            lista = resumo.get("ab_experiments") or []
            resumo["ab"] = lista[int(exp)]["ab"] if int(exp) < len(lista) else None
        v = veredito([resumo], None,
                     json.loads(Path(imagem).read_text(encoding="utf-8")) if imagem else None,
                     json.loads(Path(ruido).read_text(encoding="utf-8")) if ruido and Path(ruido).exists() else None)
        destino = Path(args[0])
    pasta = destino if destino.is_dir() else destino.parent
    texto = md(v)
    sufixo = f"-{exp}" if exp is not None else ""
    (pasta / f"veredito{sufixo}.md").write_text(texto, encoding="utf-8")
    (pasta / f"veredito{sufixo}.json").write_text(json.dumps(v, ensure_ascii=False, indent=1), encoding="utf-8")
    print(texto, end="")
    if registrar:
        est = v.get("efeito") or {}
        with open(registrar, "a", encoding="utf-8") as f:
            f.write(json.dumps({
                "data": time.strftime("%Y-%m-%d %H:%M"), "chave": chave or "|".join(filter(None, v.get("planos") or [])),
                "sha": sha or "", "estado": v["estado"], "efeito_pct": est.get("efeito_pct"),
                "ic95_pct": est.get("ic95_pct"), "unidades": est.get("pares", est.get("execucoes")),
                "sessao": str(pasta),
            }, ensure_ascii=False) + "\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
