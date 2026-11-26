"""AutoPilot Mode: roda o título escondido e sem som por um tempo fixo e guarda os logs fora do repositório.

Uso:
  python scripts/autopilot.py rodar <nome> [--segundos 480] [--jogo <pasta ou .exe>] [CHAVE=valor ...]
  python scripts/autopilot.py confirmar

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


def rodar(args: list[str]) -> int:
    if not CONFIRMACAO.exists():
        sys.exit("Rode antes: python scripts/autopilot.py confirmar (docs/autopilot.md, seção Lei e limites).")
    nome, segundos, jogo, extra = None, 480, None, {}
    it = iter(args)
    for a in it:
        if a == "--segundos":
            segundos = int(next(it))
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
            while p.poll() is None and time.time() - inicio < segundos:
                silenciar()
                time.sleep(1)
            fim = "saiu sozinho" if p.poll() is not None else "encerrado no tempo"
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
    print(__doc__)
    sys.exit(2)
