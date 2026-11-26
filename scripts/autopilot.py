"""AutoPilot Mode: roda o título escondido e sem som por um tempo fixo e guarda os logs fora do repositório.

Uso:
  python scripts/autopilot.py rodar <nome> [--segundos 480] [--amostras 60] [--jogo <pasta ou .exe>] [CHAVE=valor ...]
  python scripts/autopilot.py aquecer [--jogo <pasta ou .exe>] [CHAVE=valor ...]
  python scripts/autopilot.py confirmar

- `aquecer` traduz para o cache, antes de abrir o título e em todos os núcleos, os shaders que as execuções
  anteriores usaram (`victory translate --warm`). Rode-o depois de mudar o tradutor ou uma opção dele: a primeira
  execução seguinte não perde minutos traduzindo durante o jogo. Passe as mesmas CHAVE=valor da execução.
- `rodar` para sozinho quando já tem `--amostras` linhas de quadro no mundo (padrão 60, 0 para usar só o tempo).

- `confirmar` registra, fora do repositório, que a pessoa declarou usar a própria cópia, obtida legalmente. O
  AutoPilot não roda sem isso (docs/autopilot.md).
- O título vem de `--jogo`, de `SPARK_JOGO` ou do `game_dir` de `victory.toml`.
- Logs, `resumo.md` e capturas ficam em `SPARK_LOG_DIR` (padrão `%LOCALAPPDATA%\\VItoriaSpark\\execucoes`), nunca
  dentro do repositório.
- A janela fica fora da tela (`VICTORY_HIDDEN_WINDOW=1`) e as sessões de áudio do processo são silenciadas (precisa
  de `pip install pycaw`).
- Só aperta A (Enter) para passar das telas iniciais. Uma execução por vez (trava na pasta de logs).
"""

import os
import re
import subprocess
import sys
import time
import tomllib
from pathlib import Path

RAIZ = Path(__file__).resolve().parent.parent
EXE = Path(os.environ.get("VICTORY_EXE", RAIZ / "target" / "diag" / "victory.exe"))
BASE = Path(os.environ.get("SPARK_LOG_DIR", Path(os.environ.get("LOCALAPPDATA", Path.home())) / "VItoriaSpark" / "execucoes"))
CONFIRMACAO = BASE.parent / "autopilot-confirmado.txt"


def dentro_do_repo(caminho: Path) -> bool:
    return any((p / ".victory-root").exists() for p in [caminho, *caminho.parents])


def achar_jogo(arg: str | None) -> Path:
    alvo = arg or os.environ.get("SPARK_JOGO")
    if not alvo and (RAIZ / "victory.toml").exists():
        alvo = tomllib.loads((RAIZ / "victory.toml").read_text(encoding="utf-8")).get("game_dir")
    if not alvo:
        sys.exit("Informe o título: --jogo <pasta ou .exe>, SPARK_JOGO ou game_dir em victory.toml.")
    caminho = Path(alvo)
    if caminho.is_file():
        return caminho
    config = caminho / "MicrosoftGame.config"
    if config.exists():
        nomes = re.findall(r'<Executable[^>]*Name="([^"]+)"', config.read_text(encoding="utf-8", errors="replace"))
        if len(nomes) == 1:
            return caminho / nomes[0]
    exes = list(caminho.glob("*.exe"))
    if len(exes) == 1:
        return exes[0]
    sys.exit("Não deu para escolher o executável: passe o .exe em --jogo.")


def silenciar() -> None:
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


QUADRO = re.compile(rb"[0-9.]+ fps, [0-9.]+ ms/frame \(.*?([0-9]+) draws/frame")


def amostras_do_mundo(log: Path, desde: int) -> tuple[int, int]:
    """Bytes lidos do log e quantas linhas de quadro no mundo (500+ desenhos) apareceram depois de `desde`."""
    try:
        with open(log, "rb") as f:
            f.seek(desde)
            dados = f.read()
    except OSError:
        return desde, 0
    fim = dados.rfind(b"\n") + 1
    novas = sum(1 for m in QUADRO.finditer(dados[:fim]) if int(m.group(1)) >= 500)
    return desde + fim, novas


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
        texto = log.read_text(encoding="utf-8", errors="replace")
        hashes.update(HASH.findall(texto))
        wave = (re.findall(r"host wave lanes min=\d+ max=(\d+)", texto) or [wave])[-1]
        views = (re.findall(r"host view heaps hold (\d+) views", texto) or [views])[-1]
    if not hashes or wave is None or views is None:
        sys.exit("Sem execuções anteriores com o título no mundo: rode `rodar` uma vez antes de `aquecer`.")
    usados.write_text("\n".join(sorted(hashes)) + "\n")
    env = dict(os.environ)
    env.update(VICTORY_HOST_WAVE=str(wave), VICTORY_VIEW_LIMIT=str(int(views) - 1))
    env.update(extra)
    inicio = time.time()
    r = subprocess.run([str(EXE), "translate", "--warm", str(usados), str(exe_titulo.parent)], env=env)
    print(f"aquecer: {len(hashes)} shaders conhecidos, {time.time() - inicio:.0f} s, código {r.returncode}")
    return r.returncode


def rodar(args: list[str]) -> int:
    if not CONFIRMACAO.exists():
        sys.exit("Rode antes: python scripts/autopilot.py confirmar (docs/autopilot.md, seção Lei e limites).")
    nome, segundos, amostras, jogo, extra = None, 480, 60, None, {}
    it = iter(args)
    for a in it:
        if a == "--segundos":
            segundos = int(next(it))
        elif a == "--amostras":
            amostras = int(next(it))
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
        sys.exit(f"Compile antes: cargo build --profile diag -p victory --features diagnostics ({EXE.name})")
    pasta = BASE / nome
    pasta.mkdir(parents=True, exist_ok=True)
    env = dict(os.environ)
    env.update(
        VICTORY_GAME_DIR=str(exe_titulo.parent),
        VICTORY_HIDDEN_WINDOW="1",
        VICTORY_NO_WATCHDOG="1",
        VICTORY_CAPTURE=str(pasta / "quadros"),
        VICTORY_CAPTURE_WORLD="1",
        VICTORY_CAPTURE_WORLD_EVERY="64",
        VICTORY_KEYS=",".join(f"{t}:Enter" for t in range(20, 250, 10)),
        SPARK_LOG_DIR=str(pasta),
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
                [str(EXE), "run", str(exe_titulo), "--execute"],
                env=env, stdout=saida, stderr=subprocess.STDOUT,
                creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0),
            )
            inicio = time.time()
            lidas, mundo, fim = 0, 0, None
            while p.poll() is None and time.time() - inicio < segundos:
                silenciar()
                time.sleep(1)
                lidas, novas = amostras_do_mundo(pasta / "saida.log", lidas)
                mundo += novas
                if amostras and mundo >= amostras:
                    fim = f"parado com {mundo} amostras do mundo"
                    break
            fim = fim or ("saiu sozinho" if p.poll() is not None else "encerrado no tempo")
            if p.poll() is None:
                subprocess.run(["taskkill", "/PID", str(p.pid), "/T", "/F"], capture_output=True)
                try:
                    p.wait(180)
                except subprocess.TimeoutExpired:
                    pass
    finally:
        trava.unlink(missing_ok=True)
    resumos = sorted(pasta.rglob("resumo.md"))
    print(f"{nome}: {fim} após {time.time() - inicio:.0f} s, código {p.returncode}")
    print(f"pasta: {pasta}")
    if resumos:
        print(f"resumo: {resumos[-1]}")
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
