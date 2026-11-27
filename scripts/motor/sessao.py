"""Uma execução vista pelo motor: o `resumo.json` e, numa passada só, o que o `eventos.jsonl` acrescenta.

Tudo o que os outros módulos sabem de uma execução sai daqui; nenhum deles abre arquivos de log. O que as
execuções antigas não traziam no resumo (fases do quadro, causas dos descartes, tempo de criação do dispositivo) é
recuperado das mensagens do `eventos.jsonl` quando elas existem (build de diagnóstico), e marcado com a fonte.
"""

import json
import re
import statistics
from pathlib import Path

from .conhecimento import SINAIS_DE_IMAGEM

RAIZ = Path(__file__).resolve().parent.parent.parent

# Mensagens do log de quadros (build de diagnóstico): a parte do quadro e a expressão que a tira do texto.
_QUADRO_USUAL = {
    "remap": re.compile(r"remapping: \d+ calls ([0-9.]+) ms"),
    "present": re.compile(r"in the present call ([0-9.]+)"),
    "pipeline_wait": re.compile(r"pipelines: \d+ made, \d+ loaded, ([0-9.]+) ms waited"),
    "card_idle": re.compile(r"card: idle ([0-9.]+) ms"),
    "disk": re.compile(r"disk: \d+ reads [0-9.]+ MB ([0-9.]+) worker ms"),
}
_QUADRO_MS = re.compile(r"^usual frame[^:]*: ([0-9.]+) ms")
_DESCARTE = re.compile(r"^dropped ([0-9.]+)/frame without a host pipeline: (.*)$")
_WATCH = re.compile(r"GetWriteWatch/frame ([0-9.]+) calls covering ([0-9.]+) 4 KiB pages")
_WATCH_LENTO = re.compile(r"^write-watch askings of a millisecond or more: [0-9.]+ a frame, ([0-9.]+) ms a frame")


def dentro_do_repo(caminho: Path) -> bool:
    """Se `caminho` está abaixo de uma pasta com `.victory-root` (o repositório): lá não se grava saída."""
    caminho = Path(caminho).resolve()
    return any((p / ".victory-root").exists() for p in [caminho, *caminho.parents])


def achar_resumo(caminho: str | Path) -> Path:
    """O `resumo.json` de uma sessão: o arquivo dado, ou o mais novo dentro da pasta dada."""
    p = Path(caminho)
    if p.is_dir():
        todos = sorted(p.rglob("resumo.json"), key=lambda q: q.stat().st_mtime)
        if not todos:
            raise FileNotFoundError(f"sem resumo.json em {p}")
        p = todos[-1]
    return p


def ler_resumo(caminho: str | Path) -> dict:
    """O resumo.json de uma sessão (o arquivo, ou o mais novo numa pasta)."""
    return json.loads(achar_resumo(caminho).read_text(encoding="utf-8"))


def _causa_do_descarte(texto: str) -> str:
    """A causa (nome do resumo novo) de uma linha antiga `dropped N/frame ...: <texto>`."""
    if "structure builder, which is off" in texto:
        return "builder_off"
    if texto.startswith("skipped shader"):
        return "skipped"
    if texto.startswith("refused"):
        return "refused"
    if texto == "being made":
        return "being_made"
    if texto == "no pipeline set":
        return "no_pipeline"
    if re.match(r"^(compute|vertex|pixel|mesh|amplification|geometry|hull|domain) [0-9a-f]{16}", texto):
        return "untranslated"
    return "other"


class Sessao:
    """Uma execução: `resumo` (o resumo.json) e `fatos` (o que o eventos.jsonl acrescenta, lido uma vez)."""

    def __init__(self, caminho: str | Path):
        self.arquivo = achar_resumo(caminho)
        self.pasta = self.arquivo.parent
        self.resumo: dict = json.loads(self.arquivo.read_text(encoding="utf-8"))
        self._fatos: dict | None = None

    @property
    def nome(self) -> str:
        pai = self.pasta.parent.name
        return pai.removesuffix("-sessao") or self.pasta.name

    @property
    def env(self) -> dict:
        return (self.resumo.get("session") or {}).get("env") or {}

    @property
    def offline(self) -> Path:
        """A pasta dos contêineres guardados com `SPARK_OFFLINE=1` (pode não existir)."""
        return self.pasta / "offline"

    def concorrencia(self) -> dict:
        """O que rodou junto com a execução, do `concorrencia.json` que o agendador deixa na pasta de logs dela:
        `{"modo": "exclusiva"|"intercalada", "build_s": segundos de build ao mesmo tempo}`; vazio sem o arquivo."""
        for pasta in (self.pasta, self.pasta.parent):
            try:
                dados = json.loads((pasta / "concorrencia.json").read_text(encoding="utf-8"))
            except (OSError, ValueError):
                continue
            if isinstance(dados, dict):
                return dados
        return {}

    def estavel(self) -> dict:
        return self.resumo.get("stable_scene") or {}

    def quadro_ms(self) -> float | None:
        """A mediana do quadro pela qual se compara: a da cena estável, senão a do mundo."""
        est = self.estavel().get("frames") or {}
        return est.get("p50_ms") or (self.resumo.get("frames_world") or {}).get("p50_ms")

    def parte_em_quadros_longos_pct(self) -> float:
        """Quanto do tempo no mundo foi em quadros de mais de 1 s, em %: pelas causas do resumo novo, senão
        estimado em 1,5 s por quadro longo."""
        r = self.resumo
        n = r.get("long_world_frames_over_1s", 0)
        mundo_s = max((r.get("duration_s") or 0) - (r.get("first_world_frame_s") or 0), 1.0)
        ms = sum(c["ms"] for c in r.get("long_frame_causes") or []) or n * 1500.0
        return min(ms / 1000 / mundo_s * 100, 100.0)

    def eventos(self):
        """As linhas do eventos.jsonl como dicionários (nenhuma, se o arquivo não existe)."""
        arquivo = self.pasta / "eventos.jsonl"
        if not arquivo.exists():
            return
        with open(arquivo, encoding="utf-8", errors="replace") as f:
            for linha in f:
                try:
                    yield json.loads(linha)
                except ValueError:
                    continue

    @property
    def fatos(self) -> dict:
        if self._fatos is None:
            self._fatos = self._ler_fatos()
        return self._fatos

    def _ler_fatos(self) -> dict:
        desde = self.estavel().get("from_s")
        mundo = self.resumo.get("first_world_frame_s")
        inicio = desde if desde is not None else mundo
        pedido_s = adaptador_s = None
        usual: dict[str, list[float]] = {k: [] for k in _QUADRO_USUAL}
        usual_ms: list[float] = []
        ms_estavel: list[float] = []
        fazendo = 0
        descartes: dict[str, list[float]] = {}
        watch: list[tuple[float, float]] = []
        watch_lento: list[float] = []
        vistas = None
        tem_eventos = False
        sinais: dict[str, dict] = {}
        for e in self.eventos():
            tem_eventos = True
            mensagem = e.get("message") or ""
            campos = e.get("fields") or {}
            t = e.get("t") or 0.0
            tipo = campos.get("spark.kind")
            if e.get("level") in ("WARN", "ERROR"):
                for trecho, significado in SINAIS_DE_IMAGEM:
                    if trecho in mensagem:
                        x = sinais.setdefault(significado, {"vezes": 0, "sinal": significado,
                                                            "target": e.get("target", ""), "desde_s": t})
                        x["vezes"] += 1
                        break
            if pedido_s is None and mensagem == "D3D12.X device":
                pedido_s = t
            elif tipo == "adapter" and adaptador_s is None:
                adaptador_s = t
            elif "views" in campos and mensagem.startswith("host view heaps hold"):
                vistas = campos["views"]
            if inicio is None or t < inicio:
                continue
            if tipo == "frame_rate":
                if campos.get("spark.draws", 0) >= self.estavel().get("draws_at_least", 0):
                    ms_estavel.append(campos.get("spark.ms", 0.0))
                    fazendo += campos.get("spark.making", 0) > 0
            elif mensagem.startswith("usual frame"):
                if m := _QUADRO_MS.search(mensagem):
                    usual_ms.append(float(m.group(1)))
                for nome, expr in _QUADRO_USUAL.items():
                    if m := expr.search(mensagem):
                        usual[nome].append(float(m.group(1)))
            elif m := _DESCARTE.match(mensagem):
                descartes.setdefault(_causa_do_descarte(m.group(2)), []).append(float(m.group(1)))
            elif m := _WATCH_LENTO.match(mensagem):
                watch_lento.append(float(m.group(1)))
            if m := _WATCH.search(mensagem):
                watch.append((float(m.group(1)), float(m.group(2))))
        med = statistics.median
        return {
            "tem_eventos": tem_eventos,
            "sinais_de_imagem": sinais,
            "dispositivo_s": adaptador_s - pedido_s if pedido_s is not None and adaptador_s is not None else None,
            "vistas": vistas,
            "linhas_estaveis_ms": ms_estavel,
            "linhas_fazendo_pipelines": fazendo,
            "quadro_usual": {k: med(v) for k, v in usual.items() if v},
            "quadro_usual_ms": med(usual_ms) if usual_ms else None,
            "descartes": {k: med(v) for k, v in descartes.items()},
            "watch_chamadas": med([w[0] for w in watch]) if watch else None,
            "watch_paginas": med([w[1] for w in watch]) if watch else None,
            "watch_lento_ms": med(watch_lento) if watch_lento else None,
        }

    def criacao_do_dispositivo_s(self) -> float | None:
        """Segundos entre o título pedir o dispositivo e ele ficar pronto."""
        ms = (self.resumo.get("measurement") or {}).get("device_create_ms")
        return ms / 1000 if ms else self.fatos["dispositivo_s"]

    def fases(self) -> dict:
        """As fases do quadro: `{"escopo", "quadro_ms", "partes": {nome: {...}}, "fonte"}`.

        Do resumo novo (`phases`: média, p50, p95, p99 por quadro). Em execuções antigas, das medianas das janelas
        de 1 s da cena estável (ou das linhas de fps) mais o log de quadros do build de diagnóstico: aí só há
        `mean_ms` e `share_pct`, e a fonte diz `medianas`.
        """
        f = self.resumo.get("phases")
        if f:
            return {"escopo": f["scope"], "quadro_ms": f["frame_mean_ms"], "fonte": "fases",
                    "quadros": f["frames"], "partes": {p["name"]: p for p in f["parts"]}, "extra": f}
        m = self.estavel().get("window_medians") or {}
        escopo = "stable"
        if not m.get("samples"):
            m, escopo = self.resumo.get("frame_rate_medians") or {}, "world"
        quadro = m.get("ms")
        if not quadro:
            return {"escopo": escopo, "quadro_ms": None, "fonte": "nenhuma", "quadros": 0, "partes": {}, "extra": {}}
        partes = {}
        nomes = {"preparing": "preparing_ms", "copies": "copies_ms", "textures": "textures_ms", "gpu": "gpu_ms",
                 "copy_back": "copy_back_ms"}
        for nome, campo in nomes.items():
            if m.get(campo) is not None:
                partes[nome] = {"name": nome, "mean_ms": m[campo]}
        # O log de quadros mede o quadro "usual" (sem os longos): entra em proporção do seu próprio quadro.
        usual_ms = self.fatos["quadro_usual_ms"]
        for nome, valor in self.fatos["quadro_usual"].items():
            if usual_ms:
                partes[nome] = {"name": nome, "mean_ms": valor / usual_ms * quadro}
        if self.fatos["watch_lento_ms"] is not None:
            partes["write_watch"] = {"name": "write_watch", "mean_ms": self.fatos["watch_lento_ms"]}
        for p in partes.values():
            p["share_pct"] = p["mean_ms"] / quadro * 100
        extra = {"watch_calls_per_frame": self.fatos["watch_chamadas"],
                 "watch_pages_per_frame": self.fatos["watch_paginas"],
                 "copies_mb_per_frame": m.get("copies_mb"), "copy_back_mb_per_frame": m.get("copy_back_mb")}
        return {"escopo": escopo, "quadro_ms": quadro, "fonte": "medianas", "quadros": m.get("samples", 0),
                "partes": partes, "extra": extra}

    def descartes(self) -> list[dict]:
        """Os descartes por causa, por quadro: do resumo novo, senão das linhas do build de diagnóstico."""
        novo = self.resumo.get("dropped_by_cause")
        if novo:
            return novo
        de_proposito = {"builder_off", "skipped"}
        return sorted(({"cause": k, "per_frame_median": v, "on_purpose": k in de_proposito}
                       for k, v in self.fatos["descartes"].items()), key=lambda d: -d["per_frame_median"])
