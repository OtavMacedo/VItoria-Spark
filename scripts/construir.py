"""Compila e testa em uma pasta de build comum a todas as cópias de trabalho (worktrees) do repositório.

Uso:
  python scripts/construir.py diag                  # build de diagnóstico; copia o victory.exe para target/diag/
  python scripts/construir.py release               # build de uso; copia o victory.exe para target/release/
  python scripts/construir.py testar [-p crate ...] # testes; sem -p, os crates tocados pelo git diff
  python scripts/construir.py clippy [-p crate ...] # clippy -D warnings; sem -p, os crates tocados
  python scripts/construir.py status                # quem segura cada pasta e quanto espaço ocupa
  python scripts/construir.py artefato [diag|release] # caminho do executável deste commit (constrói se faltar)

Cada build bem-sucedido também é copiado para `artefatos/<commit>[-sujo-<hash>]-<perfil>/victory.exe` ao lado do
repositório (ou SPARK_ARTEFATOS): passe esse caminho em VICTORY_EXE, e um build seguinte não troca o executável de uma
execução em andamento. Um commit limpo já construído não é construído de novo.

As dependências externas (centenas de crates, as mesmas em todo worktree) compilam uma vez por perfil. Cada perfil
tem a sua pasta (`dev` para testes e clippy, `release`, `diag`) dentro de SPARK_ALVO_COMUM, por padrão a pasta
`alvo-comum` ao lado do repositório quando ela existe. Sem ela, o build usa o `target/` do próprio repositório.

O cargo já serializa dois builds na mesma pasta, mas o `victory.exe` de um worktree sobrescreveria o do outro antes
da cópia: por isso cada perfil tem uma trava (`<pasta>.lock`) segurada do início do build até a cópia.
Uma trava de processo morto é retirada sozinha.
"""

import os
import shutil
import subprocess
import sys
import time
from pathlib import Path

RAIZ = Path(__file__).resolve().parent.parent
# Perfil -> (pasta do build, pasta do executável no repositório).
PASTA_DO_PERFIL = {"dev": "dev", "release": "release", "diag": "diag"}
# Crates cujos testes só passam em um processo (estado global da placa).
UMA_THREAD = {"victory-gpu"}


def pasta_comum() -> Path | None:
    alvo = os.environ.get("SPARK_ALVO_COMUM")
    if alvo:
        return Path(alvo)
    ao_lado = RAIZ.parent / "alvo-comum"
    return ao_lado if ao_lado.is_dir() else None


def pasta_do_build(perfil: str) -> Path:
    base = pasta_comum()
    return base / PASTA_DO_PERFIL[perfil] if base else RAIZ / "target"


def vivo(pid: int) -> bool:
    if os.name == "nt":
        saida = subprocess.run(["tasklist", "/FI", f"PID eq {pid}", "/NH"], capture_output=True, text=True).stdout
        return str(pid) in saida
    try:
        os.kill(pid, 0)
    except OSError:
        return False
    return True


class Trava:
    """Trava por arquivo criado sem sobrescrever; guarda o pid e a cópia de trabalho para o `status`."""

    def __init__(self, perfil: str):
        base = pasta_comum() or (RAIZ / "target")
        base.mkdir(parents=True, exist_ok=True)
        self.caminho = base / f"{PASTA_DO_PERFIL[perfil]}.lock"

    def __enter__(self):
        avisou = False
        while True:
            try:
                with open(self.caminho, "x") as f:
                    f.write(f"{os.getpid()} {RAIZ.name}\n")
                return self
            except FileExistsError:
                dono = self.dono()
                if dono is not None and not vivo(dono[0]):
                    self.caminho.unlink(missing_ok=True)
                    continue
                if not avisou:
                    print(f"esperando a pasta de build ({self.caminho.name}), com {dono[1] if dono else '?'}", flush=True)
                    avisou = True
                time.sleep(3)

    def __exit__(self, *_):
        self.caminho.unlink(missing_ok=True)

    def dono(self):
        try:
            pid, nome = self.caminho.read_text().split(maxsplit=1)
            return int(pid), nome.strip()
        except (OSError, ValueError):
            return None


def crates_tocados() -> list[str]:
    """Crates do workspace com mudança no diff (contra a main e no índice/árvore); os que dependem deles são
    pegos pelo próprio cargo ao compilar, mas só o código do crate tocado é testado."""
    saida = subprocess.run(
        ["git", "diff", "--name-only", "HEAD"], cwd=RAIZ, capture_output=True, text=True, check=True
    ).stdout.split()
    saida += subprocess.run(
        ["git", "ls-files", "--others", "--exclude-standard"], cwd=RAIZ, capture_output=True, text=True, check=True
    ).stdout.split()
    crates = set()
    for arq in saida:
        partes = arq.split("/")
        if len(partes) > 2 and partes[0] in ("crates", "apps"):
            crates.add(nome_do_crate(RAIZ / partes[0] / partes[1]))
        elif partes[0] in ("Cargo.toml", "Cargo.lock"):
            return []
    return sorted(crates)


def nome_do_crate(pasta: Path) -> str:
    for linha in (pasta / "Cargo.toml").read_text(encoding="utf-8").splitlines():
        if linha.startswith("name"):
            return linha.split("=")[1].strip().strip('"')
    return pasta.name


def trocar_de_copia(perfil: str) -> None:
    """Na pasta comum, o cargo identifica os crates do workspace pelo caminho relativo ao workspace, igual em todas
    as cópias de trabalho, e decide se estão em dia pela hora dos arquivos: uma cópia cujos arquivos são mais
    velhos que o último build de OUTRA cópia levaria os crates dela (visto: um build de outro commit terminou em
    0,2 s). Quando a cópia muda, os fontes desta são tocados, e o cargo refaz os crates do workspace (as
    dependências externas continuam compartilhadas). Chamado com a trava do perfil."""
    base = pasta_comum()
    if not base:
        return
    marca = base / f"{PASTA_DO_PERFIL[perfil]}.copia"
    try:
        anterior = marca.read_text(encoding="utf-8").strip()
    except OSError:
        anterior = ""
    if anterior == str(RAIZ):
        return
    agora = time.time()
    for pasta in ("crates", "apps", "xtask"):
        for arq in (RAIZ / pasta).rglob("*"):
            if arq.is_file() and "target" not in arq.parts:
                os.utime(arq, (agora, agora))
    for arq in ("Cargo.toml", "Cargo.lock"):
        if (RAIZ / arq).exists():
            os.utime(RAIZ / arq, (agora, agora))
    marca.parent.mkdir(parents=True, exist_ok=True)
    marca.write_text(str(RAIZ), encoding="utf-8")
    print(f"pasta de build {PASTA_DO_PERFIL[perfil]} usada por outra cópia: fontes desta tocados, crates do workspace refeitos")


def cargo(perfil: str, args: list[str]) -> int:
    trocar_de_copia(perfil)
    env = dict(os.environ)
    env["CARGO_TARGET_DIR"] = str(pasta_do_build(perfil))
    env.setdefault("VICTORY_CACHE_DIR", os.environ.get("VICTORY_CACHE_DIR", ""))
    if not env["VICTORY_CACHE_DIR"]:
        del env["VICTORY_CACHE_DIR"]
    # `--locked` logo depois do subcomando: o que vem depois de `--` é do executável (clippy, testes).
    return subprocess.run(["cargo", args[0], "--locked", *args[1:]], cwd=RAIZ, env=env).returncode


def identidade() -> str:
    """O commit da cópia de trabalho; com mudanças não commitadas, mais um hash delas (`<sha>-sujo-<hash>`)."""
    import hashlib
    sha = subprocess.run(["git", "rev-parse", "--short=12", "HEAD"], cwd=RAIZ, capture_output=True, text=True,
                         check=True).stdout.strip()
    diff = subprocess.run(["git", "diff", "HEAD"], cwd=RAIZ, capture_output=True, check=True).stdout
    novos = subprocess.run(["git", "ls-files", "--others", "--exclude-standard"], cwd=RAIZ, capture_output=True,
                           check=True).stdout
    for nome in novos.split():
        arq = RAIZ / nome.decode()
        if arq.is_file():
            diff += nome + arq.read_bytes()
    return sha if not diff else f"{sha}-sujo-{hashlib.sha1(diff).hexdigest()[:8]}"


def pasta_de_artefatos() -> Path:
    """Onde ficam os executáveis por identidade: SPARK_ARTEFATOS, ou `artefatos` ao lado do repositório."""
    return Path(os.environ.get("SPARK_ARTEFATOS", RAIZ.parent / "artefatos"))


def artefato(perfil: str) -> Path:
    return pasta_de_artefatos() / f"{identidade()}-{perfil}" / "victory.exe"


def construir(perfil: str) -> int:
    alvo = artefato(perfil)
    if alvo.exists() and "-sujo-" not in alvo.parent.name:
        print(f"já construído: {alvo}")
        return 0
    with Trava(perfil):
        if perfil == "diag":
            args = ["build", "--profile", "diag", "-p", "victory", "--features", "diagnostics"]
        else:
            args = ["build", "--release", "-p", "victory"]
        codigo = cargo(perfil, args)
        if codigo == 0:
            origem = pasta_do_build(perfil) / perfil / "victory.exe"
            destino = RAIZ / "target" / perfil / "victory.exe"
            if origem != destino:
                destino.parent.mkdir(parents=True, exist_ok=True)
                shutil.copy2(origem, destino)
                print(f"copiado para {destino.relative_to(RAIZ)}")
            # Uma cópia que não muda: uma execução usa esta, e um build seguinte não a troca por baixo.
            alvo.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(origem, alvo)
            print(f"artefato: {alvo}")
        return codigo


def escolher(args: list[str]) -> list[str]:
    if "-p" in args or "--workspace" in args:
        return []
    return [x for c in crates_tocados() for x in ("-p", c)]


def testar(args: list[str]) -> int:
    pacotes = args if ("-p" in args or "--workspace" in args) else escolher(args) + args
    if not pacotes:
        pacotes = ["--workspace"]
    nomes = {pacotes[i + 1] for i, a in enumerate(pacotes) if a == "-p"}
    um_so = bool(nomes & UMA_THREAD) or "--workspace" in pacotes
    with Trava("dev"):
        if "--workspace" in pacotes:
            # Como na CI: a placa só aguenta uma thread de teste, o resto roda solto.
            codigo = cargo("dev", ["test", "--workspace", "--exclude", "victory-gpu"])
            if codigo:
                return codigo
            return cargo("dev", ["test", "-p", "victory-gpu", "--", "--test-threads=1"])
        separado = [x for n in sorted(nomes - UMA_THREAD) for x in ("-p", n)]
        if separado:
            codigo = cargo("dev", ["test", *separado])
            if codigo:
                return codigo
        if nomes & UMA_THREAD:
            return cargo("dev", ["test", "-p", "victory-gpu", "--", "--test-threads=1"])
        return 0 if um_so else cargo("dev", ["test", *pacotes])


def clippy(args: list[str]) -> int:
    pacotes = args if ("-p" in args or "--workspace" in args) else (escolher(args) + args) or ["--workspace"]
    with Trava("dev"):
        return cargo("dev", ["clippy", *pacotes, "--all-targets", "--", "-D", "warnings"])


def status() -> int:
    base = pasta_comum()
    if not base:
        print("sem pasta comum: cada worktree usa o próprio target/")
        return 0
    for perfil in PASTA_DO_PERFIL:
        pasta = base / PASTA_DO_PERFIL[perfil]
        tamanho = sum(f.stat().st_size for f in pasta.rglob("*") if f.is_file()) if pasta.exists() else 0
        dono = Trava(perfil).dono()
        print(f"{perfil:8} {tamanho / 2**30:6.1f} GB  {'livre' if dono is None else f'{dono[1]} (pid {dono[0]})'}")
    return 0


def main() -> int:
    if len(sys.argv) < 2:
        print(__doc__)
        return 2
    comando, resto = sys.argv[1], sys.argv[2:]
    if comando in ("diag", "release"):
        return construir(comando)
    if comando == "testar":
        return testar(resto)
    if comando == "clippy":
        return clippy(resto)
    if comando == "status":
        return status()
    if comando == "artefato":
        perfil = resto[0] if resto else "diag"
        alvo = artefato(perfil)
        if not alvo.exists() and construir(perfil):
            return 1
        print(alvo)
        return 0
    print(__doc__)
    return 2


if __name__ == "__main__":
    sys.exit(main())
