"""O diagnóstico como texto: curto o bastante para um assistente ler inteiro, com a evidência de cada problema."""

# Problemas mostrados por extenso; os demais ficam numa linha cada (todos estão no diagnostico.json).
POR_EXTENSO = 8
EVIDENCIAS = 5


def _chaves(p: dict) -> str | None:
    partes = []
    for c in p["chaves"]:
        if c["alternavel"]:
            partes.append(f"`{c['ab']}` (A/B na execução)")
        else:
            valor = f"={c['valor']}" if c.get("valor") else ""
            partes.append(f"`{c['nome']}{valor}` (execução própria)")
    return ", ".join(partes) or None


def diagnostico(d: dict, plano: dict | None = None, gravado: bool = True) -> str:
    """O relatório. `gravado`: o `plano.toml` está na pasta da sessão (senão diz como obtê-lo)."""
    q = d["quadro"]
    linhas = [f"# Diagnóstico: {d['sessao']} ({d['estado']})", ""]
    ad = d.get("adaptador") or {}
    if ad:
        linhas.append(f"- placa: {ad.get('name', '?')}, driver {ad.get('driver', '?')}")
    if q["p50_ms"]:
        escopo = "cena estável" if q["escopo"] == "stable" else "mundo"
        linhas.append(f"- quadro ({escopo}): p50 {q['p50_ms']:.1f} ms ({1000 / q['p50_ms']:.1f} fps)"
                      + (f", p95 {q['p95_ms']:.1f}, p99 {q['p99_ms']:.1f}" if q.get("p95_ms") else "")
                      + (f"; o limite: {q['limite']}" if q.get("limite") else ""))
    if q["fonte"] == "medianas":
        linhas.append("- fases: execução anterior às fases por quadro; os números vêm das medianas das janelas")
    if d["medicao"]:
        linhas += ["", "## Medição"
                   + (" (os números abaixo NÃO valem para julgar desempenho)" if d["estado"] == "PROBLEMA_DE_MEDICAO"
                      else ""), ""]
        for a in d["medicao"]:
            linhas.append(f"- {'**GRAVE**' if a['grave'] else 'alerta'}: {a['texto']} → {a['acao']}")
    linhas += ["", f"## Problemas, do mais caro ao mais barato ({len(d['problemas'])})", ""]
    if not d["problemas"]:
        linhas.append("Nenhum problema destacado: rode por mais tempo na cena estável.")
    for i, p in enumerate(d["problemas"], 1):
        custo = f" — {p['custo_ms']:.1f} ms/quadro ({p['parte_pct']:.0f} %)" if p.get("custo_ms") is not None else ""
        if i > POR_EXTENSO:
            linhas.append(f"{i}. {p['titulo']}{custo}: {p['evidencia'][0]}")
            continue
        linhas.append(f"{i}. **{p['titulo']}**{custo} [`{p['id']}`]")
        for e in p["evidencia"][:EVIDENCIAS]:
            linhas.append(f"   - {e}")
        linhas.append(f"   - correção sugerida: {p['sugestao']}")
        if chaves := _chaves(p):
            linhas.append(f"   - chaves a testar: {chaves}")
        if p["codigo"]:
            linhas.append(f"   - código: {', '.join('`' + c + '`' for c in p['codigo'][:4])}")
        if p["offline"]:
            linhas.append(f"   - verificável sem o título: {len(p['offline'])} shader(s) guardado(s) "
                          "(`motor.py reproduzir`)")
    if plano is not None:
        linhas += ["", "## Próxima execução", ""]
        if plano["experimentos"]:
            onde = "`plano.toml` nesta pasta" if gravado else "`motor.py plano <sessão> --gravar` deixa o `plano.toml`"
            linhas.append(f"- {len(plano['alternaveis'])} chave(s) num só plano ({onde}): "
                          + "; ".join(f"`{e['ab']}`" for e in plano["experimentos"]))
        else:
            linhas.append("- nenhuma chave alternável ataca estes problemas: o próximo passo é código")
        if plano["offline"]:
            linhas.append(f"- {len(plano['offline'])} item(ns) verificável(is) sem o título: `motor.py reproduzir`")
        elif plano["guardar_offline"]:
            linhas.append("- rode com `SPARK_OFFLINE=1` para guardar os shaders com falha e verificá-los sem o título")
    return "\n".join(linhas) + "\n"
