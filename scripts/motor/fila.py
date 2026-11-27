"""Uma fila para quem roda o título e compila na mesma máquina: dois recursos, cada um com a sua trava.

- A **placa** (`placa.lock`): uma execução do título por vez.
- A **CPU** (`cpu.lock`): um build (ou outro trabalho pesado de CPU) por vez.
- Uma execução `exclusiva` (números absolutos, comparação entre execuções; o padrão) segura as duas: nada roda
  junto. Uma execução `intercalada` (A/B dentro da execução: `SPARK_AB`/`SPARK_PLAN`) segura só a placa: um build
  pode rodar ao mesmo tempo, porque os braços do A/B alternam a cada bloco e sentem a mesma carga. O build então
  roda com prioridade baixa e metade dos núcleos.
- Um build não começa durante uma execução exclusiva, nem quando uma execução exclusiva espera há mais de
  `ENVELHECE_S`. Na fila: execuções antes de builds; em cada classe, quem espera há mais de `ENVELHECE_S`
  primeiro; depois, nos builds, o mais curto (mediana do histórico pelo nome sem o número final) e, nas execuções,
  a ordem de chegada.
- Quem espera deixa um bilhete (um JSON por trabalho); bilhetes e travas de processos mortos são retirados.
- Cada build registra o seu intervalo; `sobreposicao` diz quantos segundos de build uma execução teve, e quem
  roda o título grava isso em `concorrencia.json` na pasta de logs da execução (`gravar_concorrencia`): o motor
  não compara números absolutos de execuções com carga (`medicao.py`, achado `carga_de_build`).

A pasta da fila é `SPARK_FILA_DIR`, senão `fila/` dentro de `SPARK_LOG_DIR`, senão dentro da pasta de execuções do
AutoPilot: sempre fora do repositório.

Uso: `python scripts/motor.py fila status` e
`python scripts/motor.py fila rodar <nome> [--classe pesado|jogo] [--modo exclusiva|intercalada] -- <comando...>`;
em Python, `with fila.vez("nome", "jogo", modo="intercalada"): ...`.
"""

import json
import os
import re
import subprocess
import sys
import time
from contextlib import contextmanager
from pathlib import Path

from .sessao import dentro_do_repo

ENVELHECE_S = 20 * 60
CLASSES = ("jogo", "pesado")
MODOS = ("exclusiva", "intercalada")
# Duração suposta de um trabalho sem histórico, em segundos.
SEM_HISTORICO_S = {"pesado": 120.0, "jogo": 900.0}
ESPERA_S = 3.0


def pasta_da_fila(env: dict | None = None) -> Path:
    """Onde ficam travas, bilhetes e histórico (ver o cabeçalho). Sai com erro se cair dentro do repositório."""
    env = os.environ if env is None else env
    if env.get("SPARK_FILA_DIR"):
        pasta = Path(env["SPARK_FILA_DIR"])
    elif env.get("SPARK_LOG_DIR"):
        pasta = Path(env["SPARK_LOG_DIR"]) / "fila"
    elif os.name == "nt":
        pasta = Path(env.get("LOCALAPPDATA", Path.home())) / "VItoriaSpark" / "execucoes" / "fila"
    else:
        pasta = Path(env.get("XDG_STATE_HOME") or Path.home() / ".local" / "state") / "vitoria-spark" / "execucoes" / "fila"
    if dentro_do_repo(pasta):
        sys.exit(f"{pasta}: a fila fica fora do repositório (SPARK_FILA_DIR ou SPARK_LOG_DIR)")
    return pasta


def vivo(pid: int) -> bool:
    """Se o processo `pid` existe."""
    if pid <= 0:
        return False
    if os.name == "nt":
        import ctypes
        k = ctypes.windll.kernel32
        h = k.OpenProcess(0x1000, False, pid)  # PROCESS_QUERY_LIMITED_INFORMATION
        if not h:
            return ctypes.GetLastError() == 5  # acesso negado: existe
        try:
            codigo = ctypes.c_ulong()
            return bool(k.GetExitCodeProcess(h, ctypes.byref(codigo))) and codigo.value == 259  # STILL_ACTIVE
        finally:
            k.CloseHandle(h)
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    return True


def raiz_do_nome(nome: str) -> str:
    """O nome sem o número final: `build-3` e `build-4` são o mesmo trabalho para o histórico."""
    return re.sub(r"[-_]?\d+$", "", nome)


def estimativa(nome: str, classe: str, historico: list[dict]) -> float:
    """Duração esperada (s): a mediana do histórico dos trabalhos de mesmo nome (sem o número final)."""
    duracoes = sorted(h.get("duracao_s", 0) for h in historico
                      if h.get("classe") == classe and raiz_do_nome(h.get("nome", "")) == raiz_do_nome(nome))
    return float(duracoes[len(duracoes) // 2]) if duracoes else SEM_HISTORICO_S.get(classe, 120.0)


def ordem(fila: list[dict], agora: float, historico: list[dict]) -> list[dict]:
    """Os bilhetes na ordem de vez (regras no cabeçalho)."""
    def chave(b: dict):
        velho = agora - b["hora"] >= ENVELHECE_S
        grupo = CLASSES.index(b["classe"]) if b["classe"] in CLASSES else len(CLASSES)
        curto = estimativa(b["nome"], b["classe"], historico) if b["classe"] == "pesado" else 0.0
        return (grupo, not velho, curto, b["hora"])
    return sorted(fila, key=chave)


def pode_comecar(b: dict, fila: list[dict], placa: dict | None, cpu: dict | None, agora: float) -> bool:
    """Se o bilhete `b` pode pegar as suas travas agora. `fila` em ordem de vez; `placa` e `cpu`, os donos das
    travas (None = livre)."""
    mesma = [o for o in fila if o["classe"] == b["classe"]]
    if not mesma or mesma[0]["id"] != b["id"]:
        return False
    if b["classe"] == "jogo":
        return placa is None and (b.get("modo") == "intercalada" or cpu is None)
    if cpu is not None or (placa is not None and placa.get("modo", "exclusiva") != "intercalada"):
        return False
    return not any(o["classe"] == "jogo" and o.get("modo") != "intercalada" and agora - o["hora"] >= ENVELHECE_S
                   for o in fila)


def sobreposicao(builds: list[dict], inicio: float, fim: float) -> float:
    """Segundos de build (`{"inicio", "fim"}` cada) dentro de `inicio`..`fim`."""
    return sum(max(0.0, min(fim, c["fim"]) - max(inicio, c["inicio"])) for c in builds
               if "inicio" in c and "fim" in c)


def _linhas(arquivo: Path, ultimas: int = 400) -> list[dict]:
    out = []
    try:
        for linha in arquivo.read_text(encoding="utf-8").splitlines()[-ultimas:]:
            try:
                out.append(json.loads(linha))
            except ValueError:
                continue
    except OSError:
        pass
    return out


class Fila:
    """A fila numa pasta: travas, bilhetes, histórico e os intervalos dos builds."""

    def __init__(self, pasta: Path | None = None):
        self.pasta = Path(pasta) if pasta else pasta_da_fila()
        self.placa = self.pasta / "placa.lock"
        self.cpu = self.pasta / "cpu.lock"
        self.historico = self.pasta / "historico.jsonl"
        self.builds = self.pasta / "builds.jsonl"

    def dono(self, trava: Path) -> dict | None:
        """Quem segura a trava (None se livre); a de um processo morto é retirada."""
        try:
            dono = json.loads(trava.read_text(encoding="utf-8"))
        except OSError:
            return None
        except ValueError:
            dono = {}
        if not vivo(int(dono.get("pid", 0))):
            # Escrita pela metade por quem acabou de criá-la: dá um instante antes de tratá-la como órfã.
            try:
                if not dono and time.time() - trava.stat().st_mtime < 5:
                    return {"nome": "?", "pid": 0}
            except OSError:
                return None
            trava.unlink(missing_ok=True)
            return None
        return dono

    def bilhetes(self) -> list[dict]:
        """Os bilhetes de processos vivos (os outros são apagados)."""
        vivos = []
        for f in self.pasta.glob("bilhete-*.json"):
            try:
                b = json.loads(f.read_text(encoding="utf-8"))
            except (OSError, ValueError):
                continue
            if not vivo(int(b.get("pid", 0))):
                f.unlink(missing_ok=True)
                continue
            vivos.append(b)
        return vivos

    def _criar(self, trava: Path, nome: str, classe: str, modo: str) -> bool:
        try:
            with open(trava, "x", encoding="utf-8") as t:
                json.dump({"nome": nome, "pid": os.getpid(), "classe": classe, "modo": modo, "desde": time.time()}, t)
            return True
        except FileExistsError:
            return False

    def sobreposicao(self, inicio: float, fim: float) -> float:
        """Segundos de build que rodaram entre `inicio` e `fim`, contando o build que ainda roda."""
        builds = _linhas(self.builds)
        agora = self.dono(self.cpu)
        if agora and agora.get("classe") == "pesado":
            builds.append({"inicio": agora.get("desde", fim), "fim": fim})
        return sobreposicao(builds, inicio, fim)

    @contextmanager
    def vez(self, nome: str, classe: str = "pesado", modo: str = "exclusiva"):
        """Espera a vez e segura as travas durante o bloco: `jogo` segura a placa (e a CPU, se `exclusiva`);
        `pesado` segura a CPU. Entrega `{"inicio": hora em que começou}`."""
        if classe not in CLASSES or modo not in MODOS:
            raise ValueError(f"classe {classe!r} ou modo {modo!r} desconhecido")
        self.pasta.mkdir(parents=True, exist_ok=True)
        chegou = time.time()
        ident = f"{int(chegou * 1000)}-{os.getpid()}"
        bilhete = self.pasta / f"bilhete-{ident}.json"
        bilhete.write_text(json.dumps({"id": ident, "nome": nome, "classe": classe, "modo": modo, "pid": os.getpid(),
                                       "hora": chegou}), encoding="utf-8")
        seguras: list[Path] = []
        avisou = False
        try:
            while True:
                agora = time.time()
                fila = ordem(self.bilhetes(), agora, _linhas(self.historico))
                eu = next((b for b in fila if b["id"] == ident), None)
                if eu and pode_comecar(eu, fila, self.dono(self.placa), self.dono(self.cpu), agora):
                    if classe == "pesado":
                        if self._criar(self.cpu, nome, classe, modo):
                            seguras = [self.cpu]
                            break
                    elif self._criar(self.placa, nome, classe, modo):
                        if modo == "intercalada":
                            seguras = [self.placa]
                            break
                        if self._criar(self.cpu, nome, classe, modo):
                            seguras = [self.placa, self.cpu]
                            break
                        self.placa.unlink(missing_ok=True)
                if not avisou:
                    donos = [d["nome"] for d in (self.dono(self.placa), self.dono(self.cpu)) if d]
                    print(f"fila: {nome} esperando (travas com {', '.join(donos) or 'ninguém'})", flush=True)
                    avisou = True
                time.sleep(ESPERA_S)
        finally:
            bilhete.unlink(missing_ok=True)
        comecou = time.time()
        try:
            yield {"inicio": comecou}
        finally:
            fim = time.time()
            for t in seguras:
                t.unlink(missing_ok=True)
            with open(self.historico, "a", encoding="utf-8") as h:
                h.write(json.dumps({"nome": nome, "classe": classe, "modo": modo, "chegou": chegou,
                                    "espera_s": round(comecou - chegou), "duracao_s": round(fim - comecou)}) + "\n")
            if classe == "pesado":
                with open(self.builds, "a", encoding="utf-8") as c:
                    c.write(json.dumps({"nome": nome, "inicio": comecou, "fim": fim}) + "\n")

    def gravar_concorrencia(self, pasta_de_logs: Path, modo: str, inicio: float, fim: float | None = None) -> dict:
        """Deixa em `<pasta de logs da execução>/concorrencia.json` o modo e os segundos de build que rodaram
        junto: o que o motor lê para não comparar números absolutos de execuções com carga."""
        dados = {"modo": modo, "build_s": round(self.sobreposicao(inicio, fim or time.time()))}
        if not dentro_do_repo(pasta_de_logs):
            (Path(pasta_de_logs) / "concorrencia.json").write_text(json.dumps(dados), encoding="utf-8")
        return dados

    def status(self) -> str:
        agora = time.time()
        linhas = []
        for rotulo, trava in (("placa", self.placa), ("cpu", self.cpu)):
            d = self.dono(trava)
            linhas.append(f"{rotulo}: " + (f"{d['nome']} [{d.get('classe', '?')}, {d.get('modo', '?')}] há "
                                           f"{(agora - d.get('desde', agora)) / 60:.1f} min" if d else "livre"))
        historico = _linhas(self.historico)
        fila = ordem(self.bilhetes(), agora, historico)
        linhas.append(f"fila ({len(fila)}):")
        for i, b in enumerate(fila, 1):
            previsto = f", ~{estimativa(b['nome'], b['classe'], historico) / 60:.1f} min" if b["classe"] == "pesado" else ""
            linhas.append(f"  {i}. {b['nome']} [{b['classe']}, {b.get('modo', 'exclusiva')}{previsto}] esperando há "
                          f"{(agora - b['hora']) / 60:.1f} min")
        for h in historico[-8:]:
            linhas.append(f"  feito: {h['nome']} [{h['classe']}] esperou {h['espera_s'] / 60:.1f} min, durou "
                          f"{h['duracao_s'] / 60:.1f} min")
        return "\n".join(linhas) + "\n"


def modo_da_execucao(env: dict) -> str:
    """`intercalada` para uma execução com A/B dentro dela (`SPARK_AB`/`SPARK_PLAN`), senão `exclusiva`;
    `SPARK_FILA_MODO` manda."""
    pedido = env.get("SPARK_FILA_MODO")
    if pedido in MODOS:
        return pedido
    return "intercalada" if env.get("SPARK_AB") or env.get("SPARK_PLAN") else "exclusiva"


def comando(args: list[str]) -> int:
    """`motor.py fila status|rodar ...`."""
    if not args or args[0] not in ("status", "rodar"):
        print(__doc__)
        return 2
    fila = Fila()
    if args[0] == "status":
        print(fila.status(), end="")
        return 0
    if "--" not in args or len(args) < 2 or args[1].startswith("--"):
        print(__doc__)
        return 2
    i = args.index("--")
    opcoes = args[2:i]
    classe = opcoes[opcoes.index("--classe") + 1] if "--classe" in opcoes else "pesado"
    modo = opcoes[opcoes.index("--modo") + 1] if "--modo" in opcoes else "exclusiva"
    with fila.vez(args[1], classe, modo):
        env = dict(os.environ)
        flags = 0
        if classe == "pesado":
            # Prioridade baixa: a execução que roda junto não perde a vez para o build.
            flags = getattr(subprocess, "BELOW_NORMAL_PRIORITY_CLASS", 0)
            if fila.dono(fila.placa):
                env.setdefault("CARGO_BUILD_JOBS", str(max(1, (os.cpu_count() or 2) // 2)))
        antes = (lambda: os.nice(10)) if classe == "pesado" and os.name != "nt" else None
        return subprocess.call(args[i + 1:], env=env, creationflags=flags, preexec_fn=antes)
