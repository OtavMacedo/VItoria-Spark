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
