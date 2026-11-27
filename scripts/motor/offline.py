"""Verificação sem o título: retraduz os shaders guardados de uma execução e cria as pipelines na placa real.

Uma execução com `SPARK_OFFLINE=1` deixa em `<sessão>/offline/` o contêiner de cada shader que não traduziu ou cuja
pipeline o host recusou, e o `manifesto.jsonl` com o erro de cada um. Aqui o build atual do `victory.exe` roda
`translate --pipelines --list` sobre essa pasta, com a largura de onda e o limite de vistas da execução, e cada
item é comparado com o que falhou antes: CORRIGIDO, AINDA_FALHA ou SEM_ARQUIVO. Uma correção do tradutor é
conferida em segundos; o título só roda de novo no fim de um lote, para confirmar.

Limite: o `translate` cria cada pipeline com um estado gráfico genérico, não o do título. Uma recusa que dependa do
estado (formatos dos alvos, mistura) só se confirma na execução; as de compute são reproduzidas por inteiro.
"""

import json
import os
import re
import subprocess
from pathlib import Path

from .sessao import RAIZ, Sessao

_ARQUIVO = re.compile(r"([0-9a-f]{16})\.(\w+)\.cdxil")
_BONS = ("pipeline created", "validated")


def ler_manifesto(pasta: Path) -> list[dict]:
    """As falhas guardadas (uma por shader e tipo, a primeira de cada)."""
    arquivo = pasta / "manifesto.jsonl"
    if not arquivo.exists():
        return []
    vistos, out = set(), []
    for linha in arquivo.read_text(encoding="utf-8").splitlines():
        try:
            item = json.loads(linha)
        except ValueError:
            continue
        chave = (item.get("hash"), item.get("kind"))
        if chave not in vistos:
            vistos.add(chave)
            out.append(item)
    return out


def ler_saida(saida: str) -> dict[str, list[str]]:
    """Da saída de `translate --list`, o desfecho de cada programa por hash do contêiner (do nome do arquivo)."""
    por: dict[str, list[str]] = {}
    for linha in saida.splitlines():
        m = _ARQUIVO.search(linha)
        # A linha de resumo de cada motivo cita um programa como exemplo: não é o desfecho dele.
        if not m or "  " not in linha or "(e.g. " in linha:
            continue
        por.setdefault(m.group(1), []).append(linha.rsplit("  ", 1)[1].strip())
    return por


def julgar(manifesto: list[dict], desfechos: dict[str, list[str]]) -> list[dict]:
    """Cada falha guardada contra o desfecho de agora."""
    out = []
    for item in manifesto:
        agora = [d for d in desfechos.get(item["hash"], []) if d != "duplicate"]
        ruins = [d for d in agora if not d.startswith(_BONS)]
        if not item.get("file"):
            estado = "SEM_ARQUIVO"
        elif not agora:
            estado = "NAO_ENCONTRADO"
        elif ruins:
            estado = "AINDA_FALHA"
        else:
            estado = "CORRIGIDO"
        out.append({"hash": item["hash"], "estagio": item.get("stage"), "tipo": item.get("kind"),
                    "antes": item.get("error", ""), "agora": ruins or agora, "estado": estado,
                    "pipeline": item.get("pipeline", "")})
    return out


def achar_exe() -> Path | None:
    """O victory.exe a usar: `VICTORY_EXE`, senão o build de diagnóstico, senão o release."""
    if os.environ.get("VICTORY_EXE"):
        return Path(os.environ["VICTORY_EXE"])
    for perfil in ("diag", "release"):
        exe = RAIZ / "target" / perfil / "victory.exe"
        if exe.exists():
            return exe
    return None


def ambiente(s: Sessao, extra: dict) -> dict:
    """O ambiente do `translate`: a onda e o limite de vistas que a execução teve, e as chaves do tradutor dadas."""
    env = dict(os.environ)
    onda = (s.resumo.get("adapter") or {}).get("wave_max")
    if onda and str(onda) != "0":
        env["VICTORY_HOST_WAVE"] = env["VICTORY_HOST_MAX_WAVE"] = str(onda)
    if s.fatos["vistas"]:
        env["VICTORY_VIEW_LIMIT"] = str(int(s.fatos["vistas"]) - 1)
    env.update(extra)
    return env


def reproduzir(s: Sessao, exe: Path | None = None, extra: dict | None = None, validar: bool = False) -> dict:
    """Roda a verificação e devolve `{"estado", "itens", "contagem", "comando"}`."""
    manifesto = ler_manifesto(s.offline)
    if not manifesto:
        return {"estado": "SEM_DADOS", "itens": [], "contagem": {},
                "motivo": "a execução não guardou falhas (rode com SPARK_OFFLINE=1, ou não houve nenhuma)"}
    exe = exe or achar_exe()
    if exe is None or not exe.exists():
        return {"estado": "SEM_EXECUTAVEL", "itens": [], "contagem": {},
                "motivo": "compile antes (cargo build --profile diag -p victory --features diagnostics) ou "
                          "aponte VICTORY_EXE"}
    cmd = [str(exe), "translate", str(s.offline), "--pipelines", "--list"] + (["--validate"] if validar else [])
    r = subprocess.run(cmd, env=ambiente(s, extra or {}), capture_output=True, text=True, encoding="utf-8",
                       errors="replace")
    itens = julgar(manifesto, ler_saida(r.stdout))
    contagem: dict[str, int] = {}
    for i in itens:
        contagem[i["estado"]] = contagem.get(i["estado"], 0) + 1
    estado = "FALHOU" if r.returncode != 0 else "TUDO_CORRIGIDO" if set(contagem) <= {"CORRIGIDO"} else "RESTAM_FALHAS"
    return {"estado": estado, "itens": itens, "contagem": contagem, "codigo": r.returncode,
            "comando": " ".join(cmd[1:]), "erro": r.stderr[-400:] if r.returncode != 0 else ""}


def md(resultado: dict) -> str:
    linhas = [f"# Reprodução sem o título: {resultado['estado']}", ""]
    if resultado.get("motivo"):
        linhas.append(f"- {resultado['motivo']}")
    if resultado.get("contagem"):
        linhas.append("- " + ", ".join(f"{k} {v}" for k, v in sorted(resultado["contagem"].items())))
    if resultado.get("erro"):
        linhas.append(f"- o translate saiu com código {resultado['codigo']}: {resultado['erro'].strip()[:300]}")
    for i in resultado["itens"]:
        agora = "; ".join(dict.fromkeys(i["agora"]))[:200] or "—"
        linhas.append(f"- `{i['hash']}.{i['estagio']}` ({i['tipo']}): **{i['estado']}**; antes: {i['antes'][:120]}; "
                      f"agora: {agora}")
    if resultado["estado"] in ("TUDO_CORRIGIDO", "RESTAM_FALHAS"):
        linhas.append("- próxima ação: com tudo corrigido, uma execução de confirmação (depois de `aquecer`); as "
                      "recusas de pipelines gráficas só se confirmam nela")
    return "\n".join(linhas) + "\n"
