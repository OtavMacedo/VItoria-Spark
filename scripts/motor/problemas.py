"""O diagnóstico de uma execução: todos os problemas que ela mostra, numa lista só, do mais caro ao mais barato.

Cada problema traz a evidência (ms, %, contagens, hashes), a correção sugerida, as chaves a testar (as que alternam
dentro da execução vão para o próximo `SPARK_PLAN`), a área do código e se dá para verificar sem o título
(`motor.py reproduzir`). A ordem é por `pontos`: uma falha vale 1000; uma fase do quadro vale a sua parte do quadro
em %; os problemas de imagem valem de 15 a 60 conforme quanto trabalho deixam de fora. Assim uma execução responde
de uma vez "o que resolver, em que ordem, e como testar cada coisa".
"""

import math
import re

from . import conhecimento as K
from . import medicao
from .sessao import Sessao

# Uma fase entra na lista com esta parte do quadro (%) ou este tempo (ms por quadro).
FASE_MIN_PCT = 5.0
FASE_MIN_MS = 2.0
# Cauda: o p95 de uma fase a tantas vezes o p50, e tantos ms acima, cresce nos quadros lentos.
CAUDA_VEZES = 3.0
CAUDA_MS = 10.0
# Cópia em memória abaixo disto (MB/s) é lenta: a origem custa a ler ou faltam páginas.
COPIA_LENTA_MB_S = 1500.0
# Parte da espera pela placa com ela ociosa acima da qual a espera é fila e sincronização, não trabalho dela.
PLACA_OCIOSA_PARTE = 0.3
# Memória compartilhada em uso (MB) acima da qual a placa está transbordando para a RAM.
COMPARTILHADA_MB = 2048


def _problema(ident: str, titulo: str, pontos: float, evidencia: list[str], sugestao: str, **mais) -> dict:
    return {"id": ident, "titulo": titulo, "pontos": round(pontos, 1), "evidencia": evidencia, "sugestao": sugestao,
            "chaves": mais.pop("chaves", []), "codigo": mais.pop("codigo", []), "offline": mais.pop("offline", []),
            **mais}


def _guardados(s: Sessao) -> set[str]:
    """Os hashes com contêiner guardado para reprodução offline."""
    if not s.offline.is_dir():
        return set()
    return {f.name.split(".")[0] for f in s.offline.glob("*.cdxil")}


def _fases(s: Sessao) -> list[dict]:
    f = s.fases()
    quadro = f["quadro_ms"]
    if not quadro:
        return []
    partes = dict(f["partes"])
    # O preparo sem as partes medidas dentro dele.
    if "preparing" in partes:
        dentro = sum(partes[n]["mean_ms"] for n in ("copies", "textures", "write_watch") if n in partes)
        resto = partes["preparing"]["mean_ms"] - dentro
        partes["preparing_other"] = {"name": "preparing_other", "mean_ms": max(resto, 0.0),
                                     "share_pct": max(resto, 0.0) / quadro * 100}
    extra = f["extra"]
    out = []
    for nome, info in K.FASES.items():
        p = partes.get(nome)
        if not p or (p["share_pct"] < FASE_MIN_PCT and p["mean_ms"] < FASE_MIN_MS):
            continue
        ev = [f"{p['mean_ms']:.1f} ms por quadro, {p['share_pct']:.0f} % do quadro de {quadro:.0f} ms "
              f"({'cena estável' if f['escopo'] == 'stable' else 'mundo'}, {f['quadros']} amostras)"]
        if p.get("p50_ms") is not None:
            ev.append(f"p50 {p['p50_ms']:.1f} / p95 {p['p95_ms']:.1f} / p99 {p['p99_ms']:.1f} ms")
            if p["p95_ms"] >= CAUDA_VEZES * max(p["p50_ms"], 0.1) and p["p95_ms"] - p["p50_ms"] >= CAUDA_MS:
                ev.append("cauda: cresce muito nos quadros lentos (p95 bem acima do p50)")
        else:
            ev.append("sem percentis por quadro: execução anterior às fases (medianas das janelas e do log de quadros)")
        if nome == "copies" and extra.get("copies_mb_per_frame"):
            taxa = extra.get("copies_mb_per_s")
            ev.append(f"{extra['copies_mb_per_frame']:.1f} MB por quadro"
                      + (f" a {taxa:.0f} MB/s" if taxa else ""))
            if taxa and taxa < COPIA_LENTA_MB_S:
                ev.append("taxa baixa para cópia em memória: a origem é lenta de ler ou as páginas faltam")
        if nome == "copy_back" and extra.get("copy_back_mb_per_frame"):
            taxa = extra.get("copy_back_mb_per_s")
            ev.append(f"{extra['copy_back_mb_per_frame']:.1f} MB por quadro"
                      + (f" a {taxa:.0f} MB/s" if taxa else ""))
        if nome == "write_watch" and extra.get("watch_calls_per_frame"):
            ev.append(f"{extra['watch_calls_per_frame']:.0f} consultas por quadro sobre "
                      f"{extra.get('watch_pages_per_frame') or 0:.0f} páginas"
                      + (f", {extra['watch_us_per_call']:.0f} µs cada" if extra.get("watch_us_per_call") else ""))
        if nome == "gpu" and "card_idle" in partes:
            ociosa = partes["card_idle"]["mean_ms"]
            ev.append(f"a placa ficou ociosa {ociosa:.1f} ms por quadro: "
                      + ("boa parte da espera é fila e sincronização, não trabalho da placa"
                         if ociosa > PLACA_OCIOSA_PARTE * p["mean_ms"] else "a espera é trabalho da placa"))
        out.append(_problema(f"fase:{nome}", info["titulo"], p["share_pct"], ev, info["ideia"],
                             chaves=K.chaves_da_fase(nome, s.env), codigo=info["codigo"], o_que=info["o_que"],
                             custo_ms=round(p["mean_ms"], 2), parte_pct=round(p["share_pct"], 1)))
    return out


def _quadros_longos(s: Sessao) -> list[dict]:
    r = s.resumo
    n = r.get("long_world_frames_over_1s", 0)
    if not n:
        return []
    causas = r.get("long_frame_causes") or []
    mundo = r.get("frames_world") or {}
    parte = s.parte_em_quadros_longos_pct()
    mem = r.get("memory") or {}
    ev = [f"{n} quadros de mais de 1 s no mundo (de {mundo.get('frames', '?')}), cerca de {parte:.0f} % do tempo no "
          f"mundo; p99 {mundo.get('p99_ms', '?')} ms, máximo {mundo.get('max_ms', '?')} ms"]
    if causas:
        ev.append("por causa: " + ", ".join(f"{c['cause']} {c['frames']} ({c['ms'] / 1000:.1f} s)" for c in causas))
    ev.append(f"falhas de página por segundo nos quadros longos: {mem.get('page_faults_per_s_in_long_frames', 0):.0f} "
              f"(média {mem.get('page_faults_per_s_mean') or 0:.0f}); RAM livre mínima {mem.get('ram_free_min_mb')} MB")
    principal = causas[0]["cause"] if causas else None
    if principal in K.FASES:
        sugestao = f"a maior parte vem de `{principal}`: {K.FASES[principal]['ideia']}"
    elif principal == "page_faults" or (not causas and mem.get("page_faults_per_s_in_long_frames", 0) > 5000):
        sugestao = ("o sistema está paginando: reduzir o que a camada segura (orçamentos de texturas e de cópias na "
                    "placa) e conferir a RAM livre antes de rodar")
    else:
        sugestao = ("nenhuma fase explica esses quadros: procurar no eventos.jsonl, perto dos momentos, compilação "
                    "de pipelines e submissões lentas")
    return [_problema("quadros_longos", "Quadros de mais de 1 s", min(parte * 2, 80.0), ev, sugestao,
                      codigo=K.CODIGO["quadros_longos"],
                      chaves=[{"nome": nome, "alternavel": False, "nota": nota, "atual": s.env.get(nome)}
                              for nome, nota in K.CHAVES_DE_MEMORIA] if principal in (None, "page_faults") else [])]


def _traducao(s: Sessao) -> list[dict]:
    r = s.resumo
    guardados = _guardados(s)
    out = []
    instrucoes = r.get("untranslated_instructions") or []
    if instrucoes:
        por_instrucao: dict[str, list[dict]] = {}
        for sh in r.get("untranslated_shaders") or []:
            por_instrucao.setdefault(sh["instruction"], []).append(sh)
        itens = [{"instrucao": i["key"], "shaders": i.get("distinct_shaders", 0), "erro": i.get("detail", ""),
                  "hashes": [f"{sh['hash']}.{sh['stage']}" for sh in por_instrucao.get(i["key"], [])]}
                 for i in instrucoes]
        total = sum(i["shaders"] for i in itens)
        hashes = [h for i in itens for h in i["hashes"]]
        ev = [f"{total} shader(s) parados por {len(itens)} instrução(ões) que o tradutor não tem"]
        ev += [f"`{i['instrucao']}`: {i['shaders']} shader(s)" + (f" ({', '.join(i['hashes'][:4])})" if i["hashes"] else "")
               for i in itens[:6]]
        out.append(_problema(
            "shaders_nao_traduzidos", "Shaders que não traduzem", 30 + min(30, total), ev,
            "implementar a instrução mais usada no tradutor; os desenhos dessas pipelines somem até lá",
            codigo=K.CODIGO["shaders_nao_traduzidos"], itens=itens,
            offline=sorted({h.split(".")[0] for h in hashes} & guardados)))
    recusadas = r.get("refused_pipelines") or []
    if recusadas:
        itens = [{"shaders": p["key"], "vezes": p["count"], "motivo": p.get("detail", "")} for p in recusadas]
        onda = [i for i in itens if "waves of" in i["motivo"]]
        ev = [f"{r.get('refused_pipelines_total', len(itens))} pipeline(s) que o host recusou"]
        ev += [f"`{i['shaders']}` ({i['vezes']}x): {i['motivo'][:160]}" for i in itens[:6]]
        sugestao = "ler o motivo de cada uma; a pipeline recusada some da imagem"
        if onda:
            sugestao = (f"{len(onda)} pedem ondas mais largas que as da placa: a forma por onda larga do tradutor "
                        f"({K.CHAVE_DE_ONDA} lista os programas que a usam) é o caminho; as outras, pelo motivo")
        hashes = {h for i in itens for h in _hashes(i["shaders"])}
        out.append(_problema("pipelines_recusadas", "Pipelines recusadas pelo host", 25 + min(25, len(itens) * 3),
                             ev, sugestao, codigo=K.CODIGO["pipelines_recusadas"], itens=itens,
                             offline=sorted(hashes & guardados)))
    return out


def _hashes(texto: str) -> list[str]:
    return re.findall(r"\b[0-9a-f]{16}\b", texto)


def _descartes(s: Sessao) -> list[dict]:
    causas = s.descartes()
    if not causas:
        return []
    m = (s.estavel().get("window_medians") or {})
    if not m.get("samples"):
        m = s.resumo.get("frame_rate_medians") or {}
    desenhos = m.get("draws") or 0
    falhas = [c for c in causas if not c.get("on_purpose")]
    de_proposito = [c for c in causas if c.get("on_purpose")]
    out = []
    total = sum(c["per_frame_median"] for c in falhas)
    if total >= 1:
        ev = [f"{total:.0f} desenhos ou despachos por quadro não rodam no host (de {desenhos:.0f})"]
        ev.append("por causa: " + ", ".join(f"{c['cause']} {c['per_frame_median']:.1f}" for c in falhas))
        maior = falhas[0]["cause"]
        sugestao = {
            "being_made": "as pipelines estavam sendo compiladas: aquecer antes de medir e de julgar a imagem",
            "untranslated": "corrigir primeiro os shaders que não traduzem (item próprio da lista)",
            "refused": "corrigir primeiro as pipelines recusadas (item próprio da lista)",
            "no_pipeline": "o título desenhou sem pipeline: conferir a ordem das chamadas de lista que a definem",
            "unbound": "os recursos do despacho não puderam ser ligados: ver os avisos de vistas nulas",
        }.get(maior, "ver no eventos.jsonl (target victory_gpu::draw) o que precede os descartes")
        parte = total / desenhos * 100 if desenhos else 0.0
        out.append(_problema("descartes", "Desenhos descartados", 20 + min(40, parte * 4), ev, sugestao,
                             codigo=K.CODIGO["descartes"], causas=falhas))
    total_p = sum(c["per_frame_median"] for c in de_proposito)
    if total_p >= 1:
        out.append(_problema(
            "descartes_de_proposito", "Trabalho deixado de fora por escolha dos ajustes", 1.0,
            [f"{total_p:.0f} por quadro: " + ", ".join(f"{c['cause']} {c['per_frame_median']:.0f}"
                                                       for c in de_proposito),
             "não é falha de tradução nem de pipeline"],
            f"nada a corrigir; `{K.CHAVE_DO_CONSTRUTOR}=1` liga o construtor de estruturas de ray tracing, se a "
            "imagem pedir", codigo=[K.GPU + "pipeline.rs"]))
    return out


def _memoria(s: Sessao) -> list[dict]:
    mem = s.resumo.get("memory") or {}
    usada, orcamento = mem.get("vram_used_max_mb"), mem.get("vram_budget_min_mb")
    compartilhada = mem.get("shared_used_max_mb") or 0
    ev = []
    if usada and orcamento and usada > orcamento * 0.95:
        ev.append(f"VRAM em uso até {usada:.0f} MB para um orçamento de {orcamento:.0f} MB")
    if compartilhada > COMPARTILHADA_MB:
        ev.append(f"{compartilhada:.0f} MB de memória compartilhada (RAM usada como memória da placa: lenta)")
    if not ev:
        return []
    chaves = [{"nome": nome, "alternavel": False, "nota": nota, "atual": s.env.get(nome)}
              for nome, nota in K.CHAVES_DE_MEMORIA]
    chaves += [c for c in K.chaves_da_fase("textures", s.env) if c["alternavel"]]
    return [_problema("memoria", "Memória da placa no limite", 25.0, ev,
                      "baixar os orçamentos até a memória compartilhada parar de crescer; ela explica quadros longos "
                      "e cópias lentas", chaves=chaves, codigo=K.CODIGO["memoria"])]


def _imagem(s: Sessao) -> list[dict]:
    por_sinal: dict[str, dict] = {}
    for aviso in s.resumo.get("warnings_top") or []:
        for trecho, significado in K.SINAIS_DE_IMAGEM:
            if trecho in aviso.get("message", ""):
                x = por_sinal.setdefault(significado, {"vezes": 0, "sinal": significado, "target": aviso["target"],
                                                       "desde_s": aviso.get("first_s") or 0.0})
                x["vezes"] += aviso["count"]
                x["desde_s"] = min(x["desde_s"], aviso.get("first_s") or 0.0)
                break
    # O resumo só guarda os 10 avisos mais frequentes: o eventos.jsonl traz os outros (com o limite de repetição
    # por ponto do código, então a contagem dele é um piso).
    for significado, x in s.fatos["sinais_de_imagem"].items():
        if significado not in por_sinal:
            por_sinal[significado] = dict(x)
    sinais = sorted(por_sinal.values(), key=lambda x: -x["vezes"])
    if not sinais:
        return []
    total = sum(x["vezes"] for x in sinais)
    ev = [f"{x['vezes']}x {x['sinal']} (`{x['target']}`, desde {x['desde_s']:.0f} s)" for x in sinais]
    return [_problema("imagem", "Sinais de imagem incorreta", 15 + min(15, 5 * math.log10(max(total, 1))), ev,
                      "conferir as capturas nos momentos indicados; os sinais ligados a shaders e pipelines somem "
                      "com os itens próprios da lista", codigo=K.CODIGO["imagem"], sinais=sinais)]


def _falhas(s: Sessao) -> list[dict]:
    r = s.resumo
    out = []
    if c := r.get("crash"):
        out.append(_problema("falha", "Falha", 1000.0,
                             [f"{c.get('what')} {c.get('code') or ''} em `{c.get('location')}` aos {c.get('at_s')} s",
                              "pilha: " + " < ".join((c.get("stack") or [])[:6])],
                             "começar pela pilha e pelos últimos erros do eventos.jsonl"))
    for d in r.get("device_lost") or []:
        out.append(_problema("dispositivo_perdido", "Dispositivo perdido", 900.0,
                             [f"aos {d['at_s']:.0f} s: {d['reason'][:200]}"],
                             "ver as submissões lentas e as pipelines feitas logo antes; conferir o TdrDelay"))
        break
    if r.get("first_world_frame_s") is None and (r.get("frames_all") or {}).get("frames"):
        out.append(_problema("sem_mundo", "Não chegou ao mundo", 800.0,
                             ["nenhum trecho de quadros com 6+ submissões"],
                             "o problema está no carregamento: ver avisos e erros"))
    return out


def diagnosticar(s: Sessao) -> dict:
    """O diagnóstico da execução: estado, problemas de medição e a lista de problemas em ordem."""
    achados = medicao.verificar(s)
    problemas = _falhas(s) + _fases(s) + _quadros_longos(s) + _traducao(s) + _descartes(s) + _memoria(s) + _imagem(s)
    problemas.sort(key=lambda p: -p["pontos"])
    f = s.fases()
    partes = f["partes"]
    gpu, preparo = (partes.get(n, {}).get("mean_ms") for n in ("gpu", "preparing"))
    limite = None
    if gpu is not None and preparo is not None:
        ociosa = partes.get("card_idle", {}).get("mean_ms") or 0.0
        # Esperar a placa não é a placa trabalhar: com ela ociosa boa parte da espera, o limite é a sincronização.
        if gpu <= preparo:
            limite = "CPU (preparo)"
        elif ociosa > PLACA_OCIOSA_PARTE * gpu:
            limite = f"sincronização com a placa (ela fica ociosa {ociosa:.0f} dos {gpu:.0f} ms de espera)"
        else:
            limite = "placa"
    graves = medicao.graves(achados)
    estado = "FALHA" if any(p["pontos"] >= 800 for p in problemas) else "PROBLEMA_DE_MEDICAO" if graves else "OK"
    est = s.estavel().get("frames") or {}
    return {
        "sessao": s.nome,
        "estado": estado,
        "medicao": achados,
        "quadro": {"escopo": f["escopo"], "p50_ms": s.quadro_ms(), "p95_ms": est.get("p95_ms"),
                   "p99_ms": est.get("p99_ms"), "quadros": est.get("frames"), "limite": limite, "fonte": f["fonte"]},
        "adaptador": s.resumo.get("adapter") or {},
        "problemas": problemas,
    }
