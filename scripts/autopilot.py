"""AutoPilot Mode: roda o título escondido e sem som por um tempo fixo e guarda os logs fora do repositório.

Uso:
  python scripts/autopilot.py rodar <nome> [--segundos 900] [--amostras 60] [--precisao 3] [--jogo <pasta ou .exe>]
                                   [CHAVE=valor ...]
  python scripts/autopilot.py aquecer [--jogo <pasta ou .exe>] [CHAVE=valor ...]
  python scripts/autopilot.py confirmar

- `aquecer` traduz para o cache, antes de abrir o título e em todos os núcleos, os shaders que as execuções
  anteriores usaram (`victory translate --warm`). Rode-o depois de mudar o tradutor ou uma opção dele: a primeira
  execução seguinte não perde minutos traduzindo durante o jogo. Passe as mesmas CHAVE=valor da execução.
- `rodar` mede só a cena estável (4.500+ desenhos, 60 s depois do 1º quadro do mundo; ver `scripts/amostras.py`) e
  para sozinho quando o intervalo de 95 % da mediana do quadro cabe em ±`--precisao` % (padrão 3; 0 desliga), quando
  já tem `--amostras` linhas da cena estável (padrão 60, 0 sem teto) ou no tempo. Com `SPARK_AB=CHAVE` (A/B dentro da
  execução), para quando a diferença entre os braços é significativa ou o intervalo dela fica estreito.

- No fim, `rodar` chama o motor (`scripts/motor.py diagnosticar`): todos os problemas da execução em ordem, o
  plano da próxima (`plano.toml`) e, com A/B, o veredito. `SPARK_OFFLINE=1` vai ligado (os shaders com falha ficam
  na pasta da sessão para `motor.py reproduzir`; `SPARK_OFFLINE=0` desliga).

- `confirmar` registra, fora do repositório, que a pessoa declarou usar a própria cópia, obtida legalmente. O
  AutoPilot não roda sem isso (docs/autopilot.md).
- O título vem de `--jogo`, de `SPARK_JOGO` ou do `game_dir` de `victory.toml`.
- Logs, `resumo.md` e capturas ficam em `SPARK_LOG_DIR` (padrão `%LOCALAPPDATA%\\VItoriaSpark\\execucoes`; no Linux,
  `~/.local/state/vitoria-spark/execucoes`), nunca dentro do repositório.
- A janela fica fora da tela (`VICTORY_HIDDEN_WINDOW=1`) e as sessões de áudio do processo são silenciadas (precisa
  de `pip install pycaw`). No Linux o título roda sob o Wine por `scripts/linux/spark.sh`, com o prefixo sem driver
  de áudio durante a execução (docs/linux.md).
- Só aperta A (Enter) para passar das telas iniciais. Uma execução por vez (trava na pasta de logs).
"""

import os
import re
import subprocess
import sys
import time
import tomllib
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from amostras import Parada  # noqa: E402

RAIZ = Path(__file__).resolve().parent.parent
WINDOWS = os.name == "nt"
# Fora do Windows o victory.exe é compilado para o Windows (cargo-xwin) e roda sob o Wine, pelo spark.sh.
SPARK_SH = RAIZ / "scripts" / "linux" / "spark.sh"
if WINDOWS:
    _EXE_PADRAO = RAIZ / "target" / "diag" / "victory.exe"
    _BASE_PADRAO = Path(os.environ.get("LOCALAPPDATA", Path.home())) / "VItoriaSpark" / "execucoes"
else:
    _EXE_PADRAO = RAIZ / "target" / "x86_64-pc-windows-msvc" / "diag" / "victory.exe"
    _BASE_PADRAO = Path(os.environ.get("XDG_STATE_HOME") or Path.home() / ".local" / "state") / "vitoria-spark" / "execucoes"
EXE = Path(os.environ.get("VICTORY_EXE", _EXE_PADRAO))
BASE = Path(os.environ.get("SPARK_LOG_DIR", _BASE_PADRAO))
CONFIRMACAO = BASE.parent / "autopilot-confirmado.txt"


def dentro_do_repo(caminho: Path) -> bool:
    return any((p / ".victory-root").exists() for p in [caminho, *caminho.parents])


def game_dir_do_toml(texto: str) -> str | None:
    """O `game_dir` de um victory.toml: em `[paths]`, como no victory.example.toml, ou no topo."""
    # utf-8-sig no leitor tira o BOM que editores do Windows gravam; o executor em Rust também o aceita.
    config = tomllib.loads(texto)
    return config.get("paths", {}).get("game_dir") or config.get("game_dir")


# O atributo Name de cada Executable; o espaço antes impede casar com OverrideDisplayName e afins.
EXECUTAVEL = re.compile(r'<Executable\b[^>]*?\sName="([^"]+)"')


def executaveis_do_manifesto(texto: str) -> list[str]:
    """Os nomes de arquivo dos elementos Executable de um MicrosoftGame.config."""
    return EXECUTAVEL.findall(texto)


def achar_jogo(arg: str | None) -> Path:
    alvo = arg or os.environ.get("SPARK_JOGO")
    if not alvo and (RAIZ / "victory.toml").exists():
        alvo = game_dir_do_toml((RAIZ / "victory.toml").read_text(encoding="utf-8-sig"))
    if not alvo:
        sys.exit("Informe o título: --jogo <pasta ou .exe>, SPARK_JOGO ou game_dir em victory.toml.")
    caminho = Path(alvo)
    if caminho.is_file():
        return caminho
    # O pacote grava MicrosoftGame.Config ou .config; fora do Windows o nome distingue maiúsculas.
    config = next((c for c in caminho.glob("*") if c.name.lower() == "microsoftgame.config"), None)
    if config:
        nomes = executaveis_do_manifesto(config.read_text(encoding="utf-8-sig", errors="replace"))
        if len(nomes) == 1:
            return caminho / nomes[0]
    exes = list(caminho.glob("*.exe"))
    if len(exes) == 1:
        return exes[0]
    sys.exit("Não deu para escolher o executável: passe o .exe em --jogo.")


def victory(args: list[str]) -> list[str]:
    """Linha de comando do victory.exe: direto no Windows, pelo Wine (spark.sh) nos outros sistemas."""
    if WINDOWS:
        return [str(EXE), *args]
    return ["bash", str(SPARK_SH), "victory", *args]


def encerrar(p: subprocess.Popen) -> None:
    """Fecha a árvore de processos da execução: taskkill no Windows, wineserver -k do prefixo dedicado no Linux."""
    if WINDOWS:
        subprocess.run(["taskkill", "/PID", str(p.pid), "/T", "/F"], capture_output=True)
    else:
        subprocess.run(["bash", str(SPARK_SH), "parar"], capture_output=True)


def silenciar() -> None:
    if not WINDOWS:
        return
    try:
        from pycaw.pycaw import AudioUtilities
    except ImportError:
        return
    try:
        for sessao in AudioUtilities.GetAllSessions():
            if sessao.Process and sessao.Process.name().lower() == EXE.name.lower():
                volume = sessao.SimpleAudioVolume
                if not volume.GetMute():
                    volume.SetMute(1, None)
    except Exception:
        pass


ANSI = re.compile(r"\x1b\[[0-9;]*m")
HASH = re.compile(r"(?:hash=|\[(?:compute|vertex|pixel|mesh|amplification|geometry|hull|domain) )([0-9a-f]{16})")


def aquecer(args: list[str]) -> int:
    jogo, extra = None, {}
    it = iter(args)
    for a in it:
        if a == "--jogo":
            jogo = next(it)
        elif "=" in a:
            k, v = a.split("=", 1)
            extra[k] = v
    exe_titulo = achar_jogo(jogo)
    usados = BASE / "shaders-usados.txt"
    hashes = set(usados.read_text().split()) if usados.exists() else set()
    wave, views = None, None
    logs = sorted(BASE.glob("*/saida.log"), key=lambda q: q.stat().st_mtime)
    for log in logs:
        texto = ANSI.sub("", log.read_text(encoding="utf-8", errors="replace"))
        hashes.update(HASH.findall(texto))
        wave = (re.findall(r"host wave lanes min=\d+ max=(\d+)", texto) or [wave])[-1]
        views = (re.findall(r"host view heaps hold (\d+) views", texto) or [views])[-1]
    if not hashes or wave is None or views is None:
        sys.exit("Sem execuções anteriores com o título no mundo: rode `rodar` uma vez antes de `aquecer`.")
    usados.write_text("\n".join(sorted(hashes)) + "\n")
    env = dict(os.environ)
    if not WINDOWS:
        env["VICTORY_EXE"] = str(EXE)
    env.update(VICTORY_HOST_WAVE=str(wave), VICTORY_VIEW_LIMIT=str(int(views) - 1))
    env.update(extra)
    inicio = time.time()
    r = subprocess.run(victory(["translate", "--warm", str(usados), str(exe_titulo.parent)]), env=env)
    print(f"aquecer: {len(hashes)} shaders conhecidos, {time.time() - inicio:.0f} s, código {r.returncode}")
    return r.returncode


def rodar(args: list[str]) -> int:
    if not CONFIRMACAO.exists():
        sys.exit("Rode antes: python scripts/autopilot.py confirmar (docs/autopilot.md, seção Lei e limites).")
    nome, segundos, amostras, precisao, jogo, extra = None, 900, 60, 3.0, None, {}
    it = iter(args)
    for a in it:
        if a == "--segundos":
            segundos = int(next(it))
        elif a == "--amostras":
            amostras = int(next(it))
        elif a == "--precisao":
            precisao = float(next(it))
        elif a == "--jogo":
            jogo = next(it)
        elif "=" in a:
            k, v = a.split("=", 1)
            extra[k] = v
        else:
            nome = a
    if not nome:
        sys.exit(__doc__)
    exe_titulo = achar_jogo(jogo)
    if dentro_do_repo(BASE) or dentro_do_repo(exe_titulo):
        sys.exit("Logs e título precisam ficar fora do repositório (docs/asset-policy.md).")
    if not EXE.exists():
        if WINDOWS:
            sys.exit(f"Compile antes: cargo build --profile diag -p victory --features diagnostics ({EXE.name})")
        sys.exit(f"Compile antes: scripts/linux/spark.sh compilar diag ({EXE.name})")
    pasta = BASE / nome
    pasta.mkdir(parents=True, exist_ok=True)
    env = dict(os.environ)
    if not WINDOWS:
        env.update(VICTORY_EXE=str(EXE), SPARK_SEM_SOM="1")
    env.update(
        VICTORY_GAME_DIR=str(exe_titulo.parent),
        VICTORY_HIDDEN_WINDOW="1",
        VICTORY_NO_WATCHDOG="1",
        VICTORY_CAPTURE=str(pasta / "quadros"),
        VICTORY_CAPTURE_WORLD="1",
        VICTORY_CAPTURE_WORLD_EVERY="64",
        VICTORY_KEYS=",".join(f"{t}:Enter" for t in range(20, 250, 10)),
        SPARK_LOG_DIR=str(pasta),
        SPARK_OFFLINE="1",
    )
    env.update(extra)
    trava = BASE / ".autopilot.lock"
    while True:
        try:
            with open(trava, "x") as t:
                t.write(nome + "\n")
            break
        except FileExistsError:
            time.sleep(15)
    try:
        with open(pasta / "saida.log", "wb") as saida:
            p = subprocess.Popen(
                victory(["run", str(exe_titulo), "--execute"]),
                env=env, stdout=saida, stderr=subprocess.STDOUT,
                creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0),
            )
            inicio = time.time()
            parada = Parada(pasta / "saida.log", pasta, env, precisao=precisao, maximo=amostras)
            fim = None
            while p.poll() is None and time.time() - inicio < segundos:
                silenciar()
                time.sleep(1)
                motivo = parada.decidir()
                if motivo and (precisao > 0 or "teto" in motivo):
                    fim = motivo
                    break
            fim = fim or ("saiu sozinho" if p.poll() is not None else "encerrado no tempo")
            if p.poll() is None:
                encerrar(p)
                try:
                    p.wait(180)
                except subprocess.TimeoutExpired:
                    pass
    finally:
        trava.unlink(missing_ok=True)
    resumos = sorted(pasta.rglob("resumo.md"))
    print(f"{nome}: {fim} após {time.time() - inicio:.0f} s, código {p.returncode}")
    print(parada.estado())
    motor = [sys.executable, str(RAIZ / "scripts" / "motor.py")]
    if parada.ab and resumos:
        # O veredito (um por experimento do plano), com o piso de ruído se houver.
        ruido = BASE / "ruido.json"
        cmd = motor + ["ab", str(pasta)] + (["--ruido", str(ruido)] if ruido.exists() else [])
        for exp in range(len(parada.experimentos)) if parada.plano else [None]:
            subprocess.run(cmd + (["--exp", str(exp)] if exp is not None else []))
    print(f"pasta: {pasta}")
    if resumos:
        print(f"resumo: {resumos[-1]}")
        # O diagnóstico inteiro fica na pasta; aqui só o estado e os primeiros problemas.
        r = subprocess.run(motor + ["diagnosticar", str(pasta)], capture_output=True, text=True, encoding="utf-8",
                           errors="replace")
        print("\n".join(r.stdout.splitlines()[:24]))
        print(f"diagnóstico: {resumos[-1].parent / 'diagnostico.md'}")
    return 0


def confirmar() -> int:
    print(
        "AutoPilot Mode: declaração (fica só nesta máquina, fora do repositório)\n"
        "- Uso apenas a minha própria cópia do título, obtida legalmente.\n"
        "- Não baixo, não compartilho e não publico arquivos de jogo, chaves, firmware ou logs de execução.\n"
        "- Não contorno criptografia nem proteção de cópia.\n"
    )
    if input("Digite CONCORDO para confirmar: ").strip().upper() != "CONCORDO":
        return 1
    CONFIRMACAO.parent.mkdir(parents=True, exist_ok=True)
    CONFIRMACAO.write_text(time.strftime("%Y-%m-%d %H:%M") + " CONCORDO\n", encoding="utf-8")
    print(f"Registrado em {CONFIRMACAO}")
    return 0


if __name__ == "__main__":
    cmd = sys.argv[1:2]
    if cmd == ["rodar"]:
        sys.exit(rodar(sys.argv[2:]))
    if cmd == ["confirmar"]:
        sys.exit(confirmar())
    if cmd == ["aquecer"]:
        sys.exit(aquecer(sys.argv[2:]))
    print(__doc__)
    sys.exit(2)
