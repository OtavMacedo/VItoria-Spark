"""Do diagnóstico de uma execução, a próxima: um `SPARK_PLAN` que testa de uma vez todas as chaves candidatas.

- As chaves que alternam dentro da execução viram fatores de experimentos fatoriais (`ab = "A*B*C"`, até
  `FATORES_MAX` por experimento, na ordem dos problemas que atacam): N chaves julgadas numa execução.
- As chaves lidas uma vez ficam numa lista à parte: cada uma precisa de execuções próprias (2 de cada lado).
- Os itens verificáveis sem o título (shaders e pipelines com contêiner guardado) vão para `motor.py reproduzir`.
"""

from . import conhecimento as K

# Teto de cena estável por experimento, como `amostras.Parada`: o do A/B simples mais um tanto por fator a mais.
TETO_S = 480
TETO_POR_FATOR_S = 240


def teto_s(fatores: int) -> int:
    """Segundos de cena estável depois dos quais um experimento de `fatores` fatores para sem decidir."""
    return TETO_S + TETO_POR_FATOR_S * (max(fatores, 1) - 1)


def montar(diagnostico: dict, periodo_s: int = 20) -> dict:
    """O plano da próxima execução a partir do diagnóstico (`problemas.diagnosticar`)."""
    alternaveis, proprias, offline, vistas = [], [], [], set()
    sem_arquivo = []
    for p in diagnostico["problemas"]:
        for c in p["chaves"]:
            if c["nome"] in vistas:
                continue
            vistas.add(c["nome"])
            if c["alternavel"]:
                alternaveis.append({"chave": c["nome"], "ab": c["ab"], "problema": p["id"], "atual": c.get("atual")})
            else:
                proprias.append({"chave": c["nome"], "valor": c.get("valor"), "nota": c.get("nota", ""),
                                 "problema": p["id"], "atual": c.get("atual")})
        offline += [{"hash": h, "problema": p["id"]} for h in p["offline"]]
        if p["id"] in ("shaders_nao_traduzidos", "pipelines_recusadas") and not p["offline"]:
            sem_arquivo.append(p["id"])
    experimentos = []
    for i in range(0, len(alternaveis), K.FATORES_MAX):
        lote = alternaveis[i:i + K.FATORES_MAX]
        experimentos.append({"nome": f"lote-{len(experimentos) + 1}", "ab": "*".join(c["ab"] for c in lote),
                             "fatores": len(lote), "teto_s": teto_s(len(lote))})
    return {
        "periodo_s": periodo_s,
        "experimentos": experimentos,
        "alternaveis": alternaveis,
        "execucoes_proprias": proprias,
        "offline": offline,
        "guardar_offline": sem_arquivo,
        "duracao_max_s": sum(e["teto_s"] + periodo_s for e in experimentos),
    }


def _chaves_do_fator(fator: str) -> dict[str, str]:
    """As chaves de um fator (`A+B=80/94`) com os seus dois valores (`0/1` sem valor escrito)."""
    out = {}
    for parte in fator.replace(",", "+").split("+"):
        nome, _, valores = parte.strip().partition("=")
        out[nome.strip()] = valores.strip() or "0/1"
    return out


def juntar(pedidos: list[dict], periodo_s: int | None = None) -> dict:
    """Carona: junta os experimentos de vários planos (`{"origem", "periodo_s", "experimento": [{"nome", "ab"}]}`,
    como um `SPARK_PLAN` lido) em experimentos fatoriais de até `FATORES_MAX` fatores, para que uma execução
    responda às perguntas de todos.

    Um fator pedido duas vezes entra uma vez. Dois fatores que mexem na mesma chave de modos diferentes (outros
    valores, ou junto de outras chaves) não cabem no mesmo experimento: ficam em experimentos separados e saem em
    `conflitos`. `onde` diz, para cada fator pedido, o experimento e a posição em que ele ficou (o `--exp` e a
    linha do veredito a ler). O período é o maior dos pedidos, se `periodo_s` não é dado.
    """
    fatores: list[dict] = []
    onde, conflitos = [], []
    for pedido in pedidos:
        for exp in pedido.get("experimento") or []:
            for texto in exp["ab"].split(":")[0].split("*"):
                chaves = _chaves_do_fator(texto)
                igual = next((f for f in fatores if f["chaves"] == chaves), None)
                if igual is None:
                    igual = {"chaves": chaves,
                             "ab": "+".join(n if v == "0/1" else f"{n}={v}" for n, v in chaves.items())}
                    for outro in fatores:
                        if set(outro["chaves"]) & set(chaves):
                            conflitos.append({"chaves": sorted(set(outro["chaves"]) & set(chaves)),
                                              "fatores": [outro["ab"], igual["ab"]]})
                    fatores.append(igual)
                onde.append({"origem": pedido.get("origem", ""), "experimento": exp.get("nome", ""), "fator": igual})
    experimentos: list[list[dict]] = []
    for f in fatores:
        cabe = next((e for e in experimentos if len(e) < K.FATORES_MAX
                     and not any(set(o["chaves"]) & set(f["chaves"]) for o in e)), None)
        if cabe is None:
            cabe = []
            experimentos.append(cabe)
        cabe.append(f)
    for i, e in enumerate(experimentos):
        for j, f in enumerate(e):
            f["exp"], f["posicao"] = i, j
    periodo = periodo_s or max([int(p.get("periodo_s", 20)) for p in pedidos] or [20])
    return {
        "periodo_s": periodo,
        "experimentos": [{"nome": f"carona-{i + 1}", "ab": "*".join(f["ab"] for f in e), "fatores": len(e),
                          "teto_s": teto_s(len(e))} for i, e in enumerate(experimentos)],
        "onde": [{"origem": o["origem"], "experimento": o["experimento"], "fator": o["fator"]["ab"],
                  "exp": o["fator"]["exp"], "posicao": o["fator"]["posicao"]} for o in onde],
        "conflitos": conflitos,
        "duracao_max_s": sum(teto_s(len(e)) + periodo for e in experimentos),
    }


def md_da_carona(junto: dict) -> str:
    """A carona para ler: os experimentos e onde ficou a pergunta de cada pedido."""
    linhas = [f"# Carona: {len(junto['onde'])} pergunta(s) em {len(junto['experimentos'])} experimento(s) de uma "
              f"execução (até {junto['duracao_max_s'] / 60:.0f} min de cena estável)", ""]
    for i, e in enumerate(junto["experimentos"]):
        linhas.append(f"- `--exp {i}` `{e['nome']}`: `{e['ab']}`")
    linhas.append("")
    for o in junto["onde"]:
        linhas.append(f"- {o['origem']} / {o['experimento']}: `{o['fator']}` → `motor.py ab <pasta> --exp {o['exp']}`, "
                      f"fator {o['posicao'] + 1}")
    for c in junto["conflitos"]:
        linhas.append(f"- conflito em {', '.join(c['chaves'])}: `{c['fatores'][0]}` e `{c['fatores'][1]}` ficam em "
                      "experimentos separados")
    return "\n".join(linhas) + "\n"


def toml(plano: dict) -> str:
    """O arquivo de `SPARK_PLAN` do plano (vazio sem experimentos)."""
    if not plano["experimentos"]:
        return ""
    linhas = [f"periodo_s = {plano['periodo_s']}"]
    for e in plano["experimentos"]:
        linhas += ["", "[[experimento]]", f'nome = "{e["nome"]}"', f'ab = "{e["ab"]}"']
    return "\n".join(linhas) + "\n"


def md(plano: dict, arquivo: str | None = None) -> str:
    """O plano para ler: o que a próxima execução testa e o que fica fora dela."""
    linhas = ["# Plano da próxima execução", ""]
    if plano["experimentos"]:
        n = len(plano["alternaveis"])
        linhas.append(f"## Uma execução, {n} chave(s) julgada(s) ({len(plano['experimentos'])} experimento(s), "
                      f"até {plano['duracao_max_s'] / 60:.0f} min de cena estável)")
        linhas.append("")
        for e in plano["experimentos"]:
            linhas.append(f"- `{e['nome']}`: `{e['ab']}`")
        for c in plano["alternaveis"]:
            linhas.append(f"  - `{c['chave']}` ataca `{c['problema']}`"
                          + (f" (hoje `{c['atual']}`)" if c.get("atual") is not None else " (hoje sem valor)"))
        persistentes = [c["chave"] for c in plano["alternaveis"] if c["chave"] in K.IMAGEM_SO_ENTRE_EXECUCOES]
        if persistentes:
            linhas.append(f"  - {', '.join(f'`{c}`' for c in persistentes)}: o que fazem à memória persiste entre os "
                          "blocos; esta execução julga o tempo de todas as chaves, e as capturas dela não julgam a "
                          "imagem de nenhuma")
        if arquivo:
            linhas += ["", f"Rodar: `python scripts/autopilot.py rodar plano-1 SPARK_PLAN={arquivo}`"]
        else:
            linhas += ["", "Rodar: grave o plano (`motor.py plano <sessão> --gravar`) e passe-o em `SPARK_PLAN`."]
        linhas.append("Julgar: `python scripts/motor.py ab <pasta> --exp N` (um veredito por fator).")
    else:
        linhas.append("Nenhuma chave alternável ataca os problemas desta execução: o próximo passo é código.")
    if plano["execucoes_proprias"]:
        linhas += ["", "## Chaves lidas uma vez (2 execuções de cada lado; `motor.py comparar`)", ""]
        for c in plano["execucoes_proprias"]:
            valor = f"={c['valor']}" if c.get("valor") else ""
            linhas.append(f"- `{c['chave']}{valor}` para `{c['problema']}`: {c['nota']}")
    linhas += ["", "## Verificável sem o título", ""]
    if plano["offline"]:
        linhas.append(f"{len(plano['offline'])} shader(s) com contêiner guardado: depois de mudar o tradutor, "
                      "`python scripts/motor.py reproduzir <pasta>` diz quais passaram a traduzir e a criar pipeline.")
    elif plano["guardar_offline"]:
        linhas.append("Há shaders ou pipelines com falha, mas sem contêiner guardado: rode a próxima execução com "
                      "`SPARK_OFFLINE=1` para poder verificá-los sem o título.")
    else:
        linhas.append("Nada: a execução não teve falha de tradução nem pipeline recusada.")
    return "\n".join(linhas) + "\n"
