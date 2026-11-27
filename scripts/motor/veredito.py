"""Veredito de um teste de desempenho: um estado só, com os critérios que o sustentam.

Estados: GANHO, SEM_EFEITO, INCONCLUSIVO, REGRESSAO, PROBLEMA_DE_MEDICAO. GANHO exige TODOS:
1. quadro (mediana) de B >= 3 % mais curto, com o intervalo de 95 % todo abaixo de 0 (A/B: |t| >= 3 com 6+ pares;
   ver `estat.py`);
2. p95 de B não piora mais que 5 % com confiança (no A/B, o p95 de cada bloco no mesmo modelo, |t| >= 3: o p95 de
   uma execução A/A variou 23 % entre braços iguais, então um limiar sem estatística reprovaria por acaso);
3. quadros > 1 s não aumentam (teste exato de Fisher, unilateral, 5 %);
4. nenhuma falha nem perda de dispositivo em B;
5. imagem: B não perde detalhe que A tem nem ganha artefato (se o JSON de imagem for dado: vem de um medidor local
   por braço/execução, com `malha` e `artefato?` como frações de capturas; nada do título entra no repositório).
Uma piora em 2-5 é REGRESSAO mesmo com o quadro melhor.

PROBLEMA_DE_MEDICAO: sem cena estável, sem blocos/execuções bastantes, ruído acima de 2x o piso, ou (entre
execuções) um problema grave de medição em alguma delas (`medicao.py`): só nesse caso vale abrir o log. No A/B na
execução os dois braços dividem a máquina, então memória e driver viram alerta, não bloqueio.

Fatorial (`SPARK_AB=A*B*C`): um veredito por fator, com os mesmos critérios, e as interações entre fatores. São k
comparações numa execução: cada fator decide com o limiar corrigido de `estat.t_de_decisao`, e o teste de quadros
longos de cada um usa 5 %/k.
"""

import math

from . import estat, medicao
from .sessao import Sessao

GANHO_MIN_PCT = 3.0
# Chaves cujo efeito na memória persiste depois que o braço troca (uma escrita perdida ou adiada fica velha até o
# fim da execução): no A/B na execução o braço A herda o estado do B, então a imagem por braço não vale; julgue a
# imagem com execuções separadas.
IMAGEM_SO_ENTRE_EXECUCOES = ("VICTORY_SKIP_WC_BACK", "VICTORY_MERGE_MASK")
P95_MAX_PCT = 5.0


def falhas(resumo: dict) -> list[str]:
    out = []
    if resumo.get("crash"):
        out.append("falha")
    if resumo.get("device_lost"):
        out.append("dispositivo perdido")
    return out


def _quadros(r: dict) -> dict:
    return (r.get("stable_scene") or {}).get("frames") or r.get("frames_world") or {}


def _fechar(v: dict, est: dict, estado: str) -> dict:
    """Aplica as guardas (critérios 2 a 5) ao estado estatístico."""
    c = v["criterios"]
    guardas = [k for k in ("p95", "longos", "falhas", "imagem") if not c.get(k, True)]
    if estado == "PROBLEMA_DE_MEDICAO":
        v["estado"], v["motivo"] = estado, est.get("motivo")
    elif guardas:
        v["estado"], v["motivo"] = "REGRESSAO", "falhou: " + ", ".join(guardas)
    elif estado == "GANHO" and not c["quadro"]:
        v["estado"], v["motivo"] = "SEM_EFEITO", f"ganho abaixo de {GANHO_MIN_PCT} %"
    elif estado == "CONTINUA":
        v["estado"] = "INCONCLUSIVO"
    else:
        v["estado"] = estado
    return v


def _imagem(v: dict, imagem: dict | None, planos: list) -> None:
    c = v["criterios"]
    c["imagem"] = True
    persistente = [k for k in IMAGEM_SO_ENTRE_EXECUCOES if any(k in (pl or "") for pl in planos)]
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


def _alertas_de_medicao(v: dict, sessoes: list[Sessao], bloquear: bool) -> str | None:
    """Põe os achados de medição nos alertas; devolve o motivo do bloqueio, se `bloquear` e houver um grave."""
    motivo = None
    for s in sessoes:
        for a in medicao.verificar(s):
            # Falhas já são critérios do veredito, e que a execução alternou chaves é o próprio A/B.
            if a["id"] in ("falha", "dispositivo_perdido", "ab_na_execucao"):
                continue
            v["alertas"].append(f"{s.nome}: {a['texto']}")
            if a["grave"] and bloquear and motivo is None:
                motivo = f"{s.nome}: {a['texto']}"
    return motivo


def ab(s: Sessao, exp: int | None = None, imagem: dict | None = None, ruido: dict | None = None) -> dict:
    """O veredito do A/B de uma execução (o experimento `exp` de um `SPARK_PLAN`)."""
    resumo = s.resumo
    dados = resumo.get("ab")
    if exp is not None:
        lista = resumo.get("ab_experiments") or []
        dados = lista[exp]["ab"] if exp < len(lista) else None
    v: dict = {"criterios": {}, "alertas": [], "modo": "ab"}
    if not dados:
        return {**v, "estado": "PROBLEMA_DE_MEDICAO", "motivo": "a sessão não tem A/B (SPARK_AB)"}
    if not (resumo.get("stable_scene") or {}).get("from_s"):
        return {**v, "estado": "PROBLEMA_DE_MEDICAO", "motivo": "sem cena estável"}
    if len(estat.fatores(dados)) > 1:
        return _fatorial(v, s, dados, ruido)
    est = estat.analisar(dados, teto=True)
    v["efeito"] = est
    a, b = dados.get("a") or {}, dados.get("b") or {}
    v["planos"] = [a.get("sets"), b.get("sets")]
    if ruido and est.get("sd_bloco_pct") and est["sd_bloco_pct"] > 2 * ruido.get("sd_bloco_pct", math.inf):
        return {**v, "estado": "PROBLEMA_DE_MEDICAO",
                "motivo": f"ruído por bloco {est['sd_bloco_pct']} % > 2x o piso ({ruido['sd_bloco_pct']} %)"}
    longos = (a.get("long_frames_over_1s", 0), a.get("frames", 0), b.get("long_frames_over_1s", 0),
              b.get("frames", 0))
    c = v["criterios"]
    e = est.get("efeito_pct")
    c["quadro"] = e is not None and e <= -GANHO_MIN_PCT and est.get("estado") == "GANHO"
    if "p95_t" in est:
        # O p95 por bloco no mesmo modelo; só piora com confiança (|t| >= 3) e acima do limite.
        c["p95"] = not (est["p95_t"] >= 3 and est["p95_efeito_pct"] > P95_MAX_PCT)
    else:
        pa, pb = a.get("p95_ms"), b.get("p95_ms")
        c["p95"] = not (pa and pb and (pb / pa - 1) * 100 > P95_MAX_PCT)
    p_longos = estat.fisher_mais(*longos)
    c["longos"] = p_longos >= 0.05
    falhou = falhas(resumo)
    c["falhas"] = not falhou
    _imagem(v, imagem, v["planos"])
    _alertas_de_medicao(v, [s], bloquear=False)
    v["numeros"] = {"p95_ms": (a.get("p95_ms"), b.get("p95_ms")), "longos": longos, "p_longos": round(p_longos, 3),
                    "falhas_b": falhou}
    return _fechar(v, est, est.get("estado", "INCONCLUSIVO"))


def _fatorial(v: dict, s: Sessao, dados: dict, ruido: dict | None) -> dict:
    est = estat.analisar_fatorial(dados, teto=True)
    v.update(modo="fatorial", efeito=est, fatores=[])
    if not est["fatores"]:
        return {**v, "estado": "PROBLEMA_DE_MEDICAO", "motivo": est.get("motivo", "blocos insuficientes")}
    if ruido and est.get("sd_bloco_pct", 0) > 2 * ruido.get("sd_bloco_pct", math.inf):
        return {**v, "estado": "PROBLEMA_DE_MEDICAO",
                "motivo": f"ruído por bloco {est['sd_bloco_pct']} % > 2x o piso ({ruido['sd_bloco_pct']} %)"}
    falhou = falhas(s.resumo)
    k = len(est["fatores"])
    for f in est["fatores"]:
        p_longos = estat.fisher_mais(*f["longos"])
        um = {"criterios": {
            "quadro": f["efeito_pct"] <= -GANHO_MIN_PCT and f["estado"] == "GANHO",
            "p95": not (f.get("p95_t", 0) >= est.get("t_decisao", 3) and f.get("p95_efeito_pct", 0) > P95_MAX_PCT),
            "longos": p_longos >= 0.05 / k,
            "falhas": not falhou,
        }, "alertas": []}
        _fechar(um, f, f["estado"])
        if f.get("interage_com"):
            um["motivo"] = "interage com " + ", ".join(f["interage_com"]) + ": teste os dois juntos num A/B simples"
        v["fatores"].append({**f, "estado": um["estado"], "motivo": um.get("motivo"), "p_longos": round(p_longos, 3)})
    _alertas_de_medicao(v, [s], bloquear=False)
    estados = [f["estado"] for f in v["fatores"]]
    # O estado geral serve a quem só lê uma palavra: o pior desfecho manda, um ganho basta para valer a pena ler.
    v["estado"] = next((e for e in ("REGRESSAO", "GANHO", "INCONCLUSIVO") if e in estados), "SEM_EFEITO")
    return v


def entre_execucoes(lado_a: list[Sessao], lado_b: list[Sessao], imagem: dict | None = None,
                    sem_medicao: bool = False) -> dict:
    """O veredito de B contra A com execuções inteiras (2+ de cada lado)."""
    v: dict = {"criterios": {}, "alertas": [], "modo": "execucoes"}
    est = estat.entre_execucoes([s.quadro_ms() for s in lado_a], [s.quadro_ms() for s in lado_b])
    v["efeito"] = est
    bloqueio = _alertas_de_medicao(v, lado_a + lado_b, bloquear=not sem_medicao)
    ra, rb = [s.resumo for s in lado_a], [s.resumo for s in lado_b]

    def mediana_p95(rs):
        vals = sorted(_quadros(r).get("p95_ms") or 0 for r in rs)
        return vals[len(vals) // 2] if vals else None

    p95 = (mediana_p95(ra), mediana_p95(rb))
    longos = (sum(r.get("long_world_frames_over_1s", 0) for r in ra), sum(_quadros(r).get("frames") or 0 for r in ra),
              sum(r.get("long_world_frames_over_1s", 0) for r in rb), sum(_quadros(r).get("frames") or 0 for r in rb))
    c = v["criterios"]
    e = est.get("efeito_pct")
    c["quadro"] = e is not None and e <= -GANHO_MIN_PCT and est.get("estado") == "GANHO"
    c["p95"] = not (p95[0] and p95[1] and (p95[1] / p95[0] - 1) * 100 > P95_MAX_PCT)
    p_longos = estat.fisher_mais(*longos)
    c["longos"] = p_longos >= 0.05
    falhou = [f for r in rb for f in falhas(r)]
    c["falhas"] = not falhou
    _imagem(v, imagem, [])
    v["numeros"] = {"p95_ms": p95, "longos": longos, "p_longos": round(p_longos, 3), "falhas_b": falhou}
    v["fases"] = comparar_fases(lado_a, lado_b)
    if bloqueio and est.get("estado") != "PROBLEMA_DE_MEDICAO":
        return {**v, "estado": "PROBLEMA_DE_MEDICAO", "motivo": bloqueio}
    return _fechar(v, est, est.get("estado", "INCONCLUSIVO"))


def comparar_fases(lado_a: list[Sessao], lado_b: list[Sessao]) -> list[dict]:
    """Por fase do quadro, a média de cada lado (mediana entre as execuções) e a diferença: onde a mudança agiu."""
    def lado(sessoes: list[Sessao]) -> dict[str, float]:
        por: dict[str, list[float]] = {}
        for s in sessoes:
            for nome, p in s.fases()["partes"].items():
                por.setdefault(nome, []).append(p["mean_ms"])
        return {n: sorted(vs)[len(vs) // 2] for n, vs in por.items()}

    a, b = lado(lado_a), lado(lado_b)
    out = [{"fase": n, "a_ms": round(a[n], 2), "b_ms": round(b[n], 2), "dif_ms": round(b[n] - a[n], 2)}
           for n in a if n in b]
    return sorted(out, key=lambda x: -abs(x["dif_ms"]))


def md(v: dict) -> str:
    """O veredito em até 20 linhas (mais uma por fator, num fatorial)."""
    est = v.get("efeito") or {}
    linhas = [f"# Veredito: {v['estado']}", ""]
    if v.get("motivo"):
        linhas.append(f"- motivo: {v['motivo']}")
    if v.get("planos"):
        linhas.append(f"- A: `{v['planos'][0]}` | B: `{v['planos'][1]}`")
    if v["modo"] == "fatorial":
        linhas.append(f"- fatorial: {len(v.get('fatores', []))} fatores em {est.get('blocos', 0)} blocos; ruído por "
                      f"bloco {est.get('sd_bloco_pct', '?')} %; cada fator decide com |t| >= "
                      f"{est.get('t_decisao', '?')} (corrigido para {len(v.get('fatores', []))} comparações)")
        for f in v.get("fatores", []):
            linhas.append(f"- `{f['fator']}`: **{f['estado']}** {f['efeito_pct']:+.1f} % (IC 95 % "
                          f"{f['ic95_pct'][0]:+.1f} a {f['ic95_pct'][1]:+.1f}), t {f['t']}, MDE {f['mde_pct']} %, "
                          f"quadros > 1 s {f['longos'][0]}/{f['longos'][2]}"
                          + (f"; {f['motivo']}" if f.get("motivo") else ""))
        for i in est.get("interacoes", []):
            linhas.append(f"- interação `{i['fatores'][0]}` × `{i['fatores'][1]}`: {i['efeito_pct']:+.1f} % "
                          f"(t {i['t']}): os efeitos não se somam")
        if not est.get("interacoes_testadas", True):
            linhas.append("- interações: não testadas (blocos insuficientes para o ajuste com os produtos)")
        elif not est.get("interacoes"):
            linhas.append("- interações: nenhuma com |t| >= 3 (os efeitos se somam)")
    elif "efeito_pct" in est:
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
    if v.get("fases"):
        maiores = [f"{f['fase']} {f['dif_ms']:+.1f} ms" for f in v["fases"][:4] if abs(f["dif_ms"]) >= 0.5]
        if maiores:
            linhas.append(f"- fases que mais mudaram (B − A): {', '.join(maiores)}")
    if v.get("imagem"):
        ia, ib = v["imagem"]["a"], v["imagem"]["b"]
        linhas.append(f"- imagem: detalhe/malha/artefato A {ia.get('detalhe')}/{ia.get('malha')}/{ia.get('artefato?')}"
                      f", B {ib.get('detalhe')}/{ib.get('malha')}/{ib.get('artefato?')}")
    if v["modo"] != "fatorial":
        reprovou = [k for k, ok in (v.get("criterios") or {}).items() if not ok and k != "quadro"]
        linhas.append(f"- critérios que reprovaram: {', '.join(reprovou) or 'nenhum'}")
    for a in v.get("alertas", [])[:4]:
        linhas.append(f"- alerta: {a}")
    acao = {
        "GANHO": "confirmar numa 2ª sessão independente (outra hora, outra ordem); com 2 GANHO, mesclar desligada e "
                 "propor ligar no perfil com a medição",
        "SEM_EFEITO": "registrar e arquivar",
        "INCONCLUSIVO": f"mais pares/execuções só se o ganho esperado passar de {est.get('mde_pct', '?')} %",
        "REGRESSAO": "rejeitar (ou corrigir o critério que reprovou e testar de novo)",
        "PROBLEMA_DE_MEDICAO": "ver o log (só neste caso) e repetir",
    }[v["estado"]]
    if v["modo"] == "fatorial" and v["estado"] != "PROBLEMA_DE_MEDICAO":
        acao = ("cada fator pelo seu estado: GANHO confirma num A/B simples em outra sessão; REGRESSAO e SEM_EFEITO "
                "saem do próximo plano")
    linhas.append(f"- próxima ação: {acao}")
    limite = 20 + len(v.get("fatores", []))
    return "\n".join(linhas[:limite]) + "\n"


def registros(v: dict, chave: str | None, sha: str | None, pasta: str, data: str) -> list[dict]:
    """As linhas de `decisoes.jsonl` de um veredito: uma, ou uma por fator."""
    base = {"data": data, "sha": sha or "", "sessao": pasta}
    if v["modo"] == "fatorial" and v.get("fatores"):
        return [{**base, "chave": f["fator"], "estado": f["estado"], "efeito_pct": f["efeito_pct"],
                 "ic95_pct": f["ic95_pct"], "unidades": f["blocos"]} for f in v["fatores"]]
    est = v.get("efeito") or {}
    return [{**base, "chave": chave or "|".join(filter(None, v.get("planos") or [])), "estado": v["estado"],
             "efeito_pct": est.get("efeito_pct"), "ic95_pct": est.get("ic95_pct"),
             "unidades": est.get("pares", est.get("execucoes"))}]
