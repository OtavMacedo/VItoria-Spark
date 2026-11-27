"""Quando uma execução já mediu o bastante: amostras da cena estável e parada pelo intervalo de confiança.

Usado por `scripts/autopilot.py rodar` (e por quem roda o título com um script próprio): lê o log de texto da execução
enquanto ele cresce e diz quando parar.

- Uma linha de quadros do medidor (`N fps, M ms/frame (...) D draws/frame`) é uma amostra.
- O mundo começa na primeira linha com 500+ desenhos. A **cena estável** começa na primeira linha com
  `SPARK_STABLE_DRAWS` (4.500) desenhos ou mais, passados `SPARK_STABLE_AFTER_S` (60) segundos do mundo; dali em
  diante só contam as linhas com esses desenhos (o carregamento e as telas meio carregadas ficam fora da mediana).
- Sem A/B: para quando o intervalo de 95 % da mediana do ms/quadro (por ordem, sem supor distribuição) cabe em
  ±`precisao` % (padrão 3) da mediana, com pelo menos `minimo` amostras estáveis (padrão 20, ~100 s).
- Com `SPARK_AB` (ou `CHAVE=alt`): lê os blocos do `ab` do `resumo.json` da sessão (reescrito a cada ~60 s) e aplica a
  parada sequencial de `scripts/ab_estat.py` (GANHO, REGRESSAO ou SEM_EFEITO com 6+ pares); passados `teto_ab_s`
  (480) segundos de cena estável sem decisão, para como INCONCLUSIVO.
- Fatorial (`SPARK_AB=A*B*C`, ou `ab = "A*B*C"` num plano): cada fator decide sozinho com a mesma regra; a execução
  (ou o experimento) para quando todos decidiram, e o teto cresce com o número de fatores (`motor/plano.py`).
- Com `SPARK_PLAN=<plano.toml>` (vários experimentos numa execução): a mesma regra por experimento; ao decidir um,
  escreve o número do seguinte em `<plano.toml>.atual` (o motor passa a ele na próxima troca de bloco, depois de um
  bloco de lavagem) e para a execução depois do último. `resultados` guarda a decisão estatística de cada um.
- Sem mundo: com quadros saindo e nenhum no mundo por `SPARK_SEM_MUNDO_S` (300) segundos, para (0 desliga). É o
  título parado numa tela que as teclas do script não passam.
"""

import json
import os
import re
import sys
import time
import tomllib
from datetime import datetime
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from motor.estat import analisar, fatores, intervalo_da_mediana  # noqa: E402
from motor.plano import teto_s  # noqa: E402

ANSI = re.compile(rb"\x1b\[[0-9;]*m")
LINHA = re.compile(
    rb"^(\d{4}-\d\d-\d\dT[0-9:.]+)Z.*? ([0-9.]+) fps, ([0-9.]+) ms/frame \(.*? ([0-9]+) draws/frame"
)
MUNDO_DRAWS = 500
# Sem linha de quadro por tanto tempo, a execução travou (as linhas saem a cada segundo; quadros de 5 s existem).
TRAVA_MUNDO_S = 120
TRAVA_INICIO_S = 420
# Quadros saindo e nenhum no mundo: o título parou numa tela que as teclas do script não passam (a primeira
# configuração de um perfil novo, por exemplo). O mundo chega em 60-90 s nas execuções medidas.
SEM_MUNDO_S = 300


def _num(nome: str, padrao: float, env: dict | None) -> float:
    try:
        return float((env or os.environ).get(nome, padrao))
    except ValueError:
        return padrao


class Parada:
    """Acompanha o log de texto de uma execução e decide quando parar."""

    def __init__(self, log: Path, pasta_sessao: Path | None = None, env: dict | None = None,
                 precisao: float = 3.0, minimo: int = 20, maximo: int = 0, teto_ab_s: float = 480):
        self.log = Path(log)
        self.pasta_sessao = Path(pasta_sessao) if pasta_sessao else None
        e = env or os.environ
        self.plano = Path(e["SPARK_PLAN"]) if e.get("SPARK_PLAN") else None
        self.experimentos: list[str] = []
        self.periodo_s = 20.0
        if self.plano:
            dados = tomllib.loads(self.plano.read_text(encoding="utf-8"))
            self.experimentos = [x["nome"] for x in dados["experimento"]]
            self.periodo_s = float(dados.get("periodo_s", 20))
            self.controle = Path(str(self.plano) + ".atual")
            self.controle.write_text("0", encoding="utf-8")
        self.exp = 0
        self.exp_desde_t: float | None = None
        self.resultados: list[dict] = []
        self.ab = bool(self.plano) or bool(e.get("SPARK_AB")) or any(
            k.startswith("VICTORY_") and str(v).strip() == "alt" for k, v in e.items())
        self.draws_estavel = _num("SPARK_STABLE_DRAWS", 4500, env)
        self.depois_s = _num("SPARK_STABLE_AFTER_S", 60, env)
        self.precisao = precisao / 100
        self.minimo = minimo
        self.maximo = maximo
        self.teto_ab_s = teto_ab_s
        self.ultima_t: float | None = None
        self.ab_analise: dict | None = None
        self.lidos = 0
        # Relógio da máquina da última linha de quadro lida: sem linha nova por muito tempo, a execução travou.
        self.linha_relogio: float | None = None
        self.inicio_relogio = time.time()
        self.sem_mundo_s = _num("SPARK_SEM_MUNDO_S", SEM_MUNDO_S, env)
        self.mundo_t: float | None = None
        self.estavel_t: float | None = None
        self.primeira_t: float | None = None
        self.mundo = 0
        self.amostras: list[float] = []
        self.ab_estado: dict | None = None

    def ler(self) -> None:
        """Lê o que o log ganhou desde a última leitura (só linhas inteiras)."""
        try:
            with open(self.log, "rb") as f:
                f.seek(self.lidos)
                dados = f.read()
        except OSError:
            return
        fim = dados.rfind(b"\n") + 1
        self.lidos += fim
        for linha in dados[:fim].splitlines():
            if b"draws/frame" in linha:
                self.linha(ANSI.sub(b"", linha))
                self.linha_relogio = time.time()

    def linha(self, linha: bytes) -> None:
        m = LINHA.search(linha)
        if not m:
            return
        try:
            t = datetime.fromisoformat(m.group(1).decode()).timestamp()
        except ValueError:
            return
        ms, draws = float(m.group(3)), int(m.group(4))
        self.ultima_t = t
        if self.primeira_t is None:
            self.primeira_t = t
        if draws < MUNDO_DRAWS:
            return
        self.mundo += 1
        if self.mundo_t is None:
            self.mundo_t = t
        if self.estavel_t is None and draws >= self.draws_estavel and t - self.mundo_t >= self.depois_s:
            self.estavel_t = t
        if self.estavel_t is not None and draws >= self.draws_estavel:
            self.amostras.append(ms)

    def resumo_ab(self) -> dict | None:
        if not self.pasta_sessao:
            return None
        resumos = sorted(self.pasta_sessao.rglob("resumo.json"), key=lambda q: q.stat().st_mtime)
        if not resumos:
            return None
        try:
            resumo = json.loads(resumos[-1].read_text(encoding="utf-8"))
        except (OSError, ValueError):
            return None
        if not self.plano:
            return resumo.get("ab")
        lista = resumo.get("ab_experiments") or []
        return lista[self.exp]["ab"] if self.exp < len(lista) else None

    def decidir(self) -> str | None:
        """O motivo para parar agora, ou None para continuar."""
        self.ler()
        n = len(self.amostras)
        # Travada: nenhuma linha de quadro por TRAVA_MUNDO_S depois do mundo, ou TRAVA_INICIO_S sem nenhuma.
        agora = time.time()
        if self.mundo_t is not None and self.linha_relogio and agora - self.linha_relogio > TRAVA_MUNDO_S:
            return f"TRAVOU: nenhuma linha de quadro há {agora - self.linha_relogio:.0f} s no mundo"
        if self.linha_relogio is None and agora - self.inicio_relogio > TRAVA_INICIO_S:
            return f"TRAVOU: nenhuma linha de quadro em {agora - self.inicio_relogio:.0f} s"
        if (self.sem_mundo_s and self.mundo_t is None and self.linha_relogio is not None
                and agora - self.inicio_relogio > self.sem_mundo_s):
            return (f"SEM MUNDO: {agora - self.inicio_relogio:.0f} s com quadros e nenhum no mundo "
                    f"({MUNDO_DRAWS}+ desenhos)")
        if self.maximo and n >= self.maximo and not self.ab:
            return f"parado com {n} amostras da cena estável (teto)"
        if self.ab:
            ab = self.resumo_ab()
            self.ab_estado = ab
            if not ab:
                return None
            # Medido na cena estável há mais que o teto: decide com o que tem (INCONCLUSIVO, se não fechou).
            inicio = max(filter(None, [self.estavel_t, self.exp_desde_t]), default=None)
            limite = max(self.teto_ab_s, teto_s(len(fatores(ab))))
            teto = (self.estavel_t is not None and self.ultima_t is not None and inicio is not None
                    and self.ultima_t - inicio >= limite)
            v = analisar(ab, teto=teto)
            self.ab_analise = v
            if v["estado"] == "CONTINUA":
                return None
            if v.get("fatores"):
                frase = f"fatorial {v['estado']} em {v['blocos']} blocos: " + "; ".join(
                    f"{f['fator']} {f['estado']} {f['efeito_pct']:+.1f} %" for f in v["fatores"])
            else:
                if "efeito_pct" in v:
                    efeito = f"{v['efeito_pct']:+.1f} % (IC 95 % {v['ic95_pct'][0]:+.1f} a {v['ic95_pct'][1]:+.1f})"
                else:
                    efeito = "sem estimativa"
                frase = f"A/B {v['estado']} em {v['pares']} pares: quadro de B contra A {efeito}"
            if not self.plano:
                return frase
            self.resultados.append({"experimento": self.experimentos[self.exp], **v})
            self.exp += 1
            # O motor só passa ao seguinte depois de um bloco de lavagem: o teto conta daí.
            self.exp_desde_t = (self.ultima_t or 0) + self.periodo_s
            self.controle.write_text(str(self.exp), encoding="utf-8")
            print(f"experimento {self.exp - 1} ({self.experimentos[self.exp - 1]}): {frase}", flush=True)
            if self.exp >= len(self.experimentos):
                return f"plano completo: {len(self.resultados)} experimentos"
            return None
        if n < self.minimo:
            return None
        baixo, mediana, alto = intervalo_da_mediana(self.amostras)
        if mediana > 0 and (alto - baixo) / 2 <= self.precisao * mediana:
            return (f"parado com {n} amostras da cena estável: mediana {mediana:.1f} ms/quadro, "
                    f"IC 95 % {baixo:.1f} a {alto:.1f}")
        return None

    def estado(self) -> str:
        """Uma linha para quem acompanha."""
        partes = [f"mundo: {self.mundo} linhas"]
        if self.estavel_t is not None and self.primeira_t is not None:
            partes.append(f"cena estável desde {self.estavel_t - self.primeira_t:.0f} s do 1º quadro, "
                          f"{len(self.amostras)} amostras")
            if len(self.amostras) >= 3:
                baixo, mediana, alto = intervalo_da_mediana(self.amostras)
                partes.append(f"mediana {mediana:.1f} ms (IC {baixo:.1f}-{alto:.1f})")
        return "; ".join(partes)


if __name__ == "__main__":
    # Simula a parada sobre logs já gravados: python scripts/amostras.py <log> [...]
    import sys

    for caminho in sys.argv[1:]:
        p = Parada(Path(caminho), env={})
        motivo = None
        with open(caminho, "rb") as f:
            for linha in f:
                if b"draws/frame" in linha:
                    p.linha(ANSI.sub(b"", linha))
                    if p.amostras and len(p.amostras) >= p.minimo and motivo is None:
                        baixo, mediana, alto = intervalo_da_mediana(p.amostras)
                        if (alto - baixo) / 2 <= p.precisao * mediana:
                            m = LINHA.search(ANSI.sub(b"", linha))
                            t = datetime.fromisoformat(m.group(1).decode()).timestamp() - p.primeira_t
                            motivo = f"{len(p.amostras)} amostras, {t:.0f} s do 1º quadro, mediana {mediana:.1f} ms"
        fim = ""
        if p.amostras:
            fim = f"; mediana final {intervalo_da_mediana(p.amostras)[1]:.1f} ms"
        print(f"{caminho}: {p.estado()}; pararia em: {motivo}{fim}")
