"""O motor de diagnóstico do VItoria Spark: uma execução, todos os problemas, a próxima execução planejada.

Uso:
  python scripts/motor.py diagnosticar <sessão> [--json]
      Todos os problemas da execução em ordem de custo, com evidência, correção sugerida, chaves a testar e código;
      os problemas de medição primeiro. Grava `diagnostico.md`, `diagnostico.json`, `plano.toml` e `plano.md`.
  python scripts/motor.py plano <sessão> [--periodo 20]
      Só o plano: o `SPARK_PLAN` que testa de uma vez todas as chaves candidatas (fatorial), as chaves que pedem
      execução própria e os itens verificáveis sem o título.
  python scripts/motor.py ab <sessão> [--exp N] [--imagem <json>] [--ruido <ruido.json>]
                                      [--registrar <decisoes.jsonl> [--chave <NOME>] [--sha <commit>]]
      O veredito do A/B de uma execução (`SPARK_AB`; N de `SPARK_PLAN`). Num fatorial, um veredito por fator.
  python scripts/motor.py comparar --a <sessão> [...] --b <sessão> [...] [--imagem <json>] [--sem-medicao]
                                      [--registrar <decisoes.jsonl> --chave <NOME> [--sha <commit>]]
      O veredito de B contra A com execuções inteiras (2+ de cada lado) e as fases que mais mudaram.
  python scripts/motor.py reproduzir <sessão> [--exe <victory.exe>] [--validar] [CHAVE=valor ...]
      Sem o título: retraduz os shaders que a execução guardou (`SPARK_OFFLINE=1`) e cria as pipelines na placa;
      diz quais falhas o build atual corrigiu.
  python scripts/motor.py ruido <sessão A/A> --gravar <ruido.json>
      O piso de ruído da máquina, de uma execução com os dois braços iguais.
  python scripts/motor.py consultar <decisoes.jsonl> [<NOME>]
      O que já foi decidido (antes de testar de novo).

<sessão> = a pasta de uma execução (vale a mais nova `resumo.json` dentro dela) ou o próprio `resumo.json`.
Tudo o que o motor grava fica na pasta da sessão, fora do repositório (docs/asset-policy.md).
"""

import json
import sys
import time
from pathlib import Path

from . import estat, offline, plano, problemas, relatorio, veredito
from .sessao import Sessao, dentro_do_repo


def _opcao(args: list[str], nome: str, valor: bool = True):
    """Tira `nome` (e o seu valor) de `args`; devolve o valor, True para uma opção sem valor, ou None."""
    if nome not in args:
        return None
    i = args.index(nome)
    if not valor:
        del args[i]
        return True
    if i + 1 >= len(args):
        sys.exit(f"{nome} precisa de um valor")
    v = args[i + 1]
    del args[i:i + 2]
    return v


def _json(caminho: str | None) -> dict | None:
    if not caminho or not Path(caminho).exists():
        return None
    return json.loads(Path(caminho).read_text(encoding="utf-8"))


def _gravar(pasta: Path, arquivos: dict[str, str]) -> None:
    """Grava os arquivos na pasta da sessão; nunca dentro do repositório."""
    if dentro_do_repo(pasta):
        print("(nada gravado: a sessão está dentro do repositório; docs/asset-policy.md)", file=sys.stderr)
        return
    for nome, texto in arquivos.items():
        if texto:
            (pasta / nome).write_text(texto, encoding="utf-8")


def _registrar(caminho: str | None, v: dict, chave: str | None, sha: str | None, pasta: Path) -> None:
    if not caminho:
        return
    with open(caminho, "a", encoding="utf-8") as f:
        for linha in veredito.registros(v, chave, sha, str(pasta), time.strftime("%Y-%m-%d %H:%M")):
            f.write(json.dumps(linha, ensure_ascii=False) + "\n")


def diagnosticar(args: list[str]) -> int:
    so_json = _opcao(args, "--json", valor=False)
    periodo = int(_opcao(args, "--periodo") or 20)
    if not args:
        sys.exit(__doc__)
    s = Sessao(args[0])
    d = problemas.diagnosticar(s)
    p = plano.montar(d, periodo)
    d["plano"] = p
    arquivo_plano = s.pasta / "plano.toml"
    texto = relatorio.diagnostico(d, p)
    _gravar(s.pasta, {"diagnostico.md": texto, "diagnostico.json": json.dumps(d, ensure_ascii=False, indent=1),
                      "plano.toml": plano.toml(p), "plano.md": plano.md(p, str(arquivo_plano))})
    print(json.dumps(d, ensure_ascii=False) if so_json else texto, end="" if not so_json else "\n")
    return 0


def planejar(args: list[str]) -> int:
    periodo = int(_opcao(args, "--periodo") or 20)
    if not args:
        sys.exit(__doc__)
    s = Sessao(args[0])
    p = plano.montar(problemas.diagnosticar(s), periodo)
    arquivo = s.pasta / "plano.toml"
    texto = plano.md(p, str(arquivo))
    _gravar(s.pasta, {"plano.toml": plano.toml(p), "plano.md": texto})
    print(texto, end="")
    return 0


def ab(args: list[str]) -> int:
    exp = _opcao(args, "--exp")
    imagem, ruido = _json(_opcao(args, "--imagem")), _json(_opcao(args, "--ruido"))
    registrar, chave, sha = (_opcao(args, n) for n in ("--registrar", "--chave", "--sha"))
    if not args:
        sys.exit(__doc__)
    s = Sessao(args[0])
    v = veredito.ab(s, int(exp) if exp is not None else None, imagem, ruido)
    texto = veredito.md(v)
    sufixo = f"-{exp}" if exp is not None else ""
    _gravar(s.pasta, {f"veredito{sufixo}.md": texto,
                      f"veredito{sufixo}.json": json.dumps(v, ensure_ascii=False, indent=1)})
    print(texto, end="")
    _registrar(registrar, v, chave, sha, s.pasta)
    return 0


def comparar(args: list[str]) -> int:
    imagem = _json(_opcao(args, "--imagem"))
    sem_medicao = bool(_opcao(args, "--sem-medicao", valor=False))
    # Aceito e ignorado: entre execuções o piso de ruído do A/B não se aplica.
    _opcao(args, "--ruido")
    registrar, chave, sha = (_opcao(args, n) for n in ("--registrar", "--chave", "--sha"))
    if "--a" not in args or "--b" not in args:
        sys.exit(__doc__)
    ia, ib = args.index("--a"), args.index("--b")
    lado_a = args[ia + 1:ib] if ia < ib else args[ia + 1:]
    lado_b = args[ib + 1:] if ib > ia else args[ib + 1:ia]
    sa, sb = [Sessao(c) for c in lado_a], [Sessao(c) for c in lado_b]
    v = veredito.entre_execucoes(sa, sb, imagem, sem_medicao)
    texto = veredito.md(v)
    destino = Path(lado_b[0])
    pasta = destino if destino.is_dir() else destino.parent
    _gravar(pasta, {"veredito.md": texto, "veredito.json": json.dumps(v, ensure_ascii=False, indent=1)})
    print(texto, end="")
    _registrar(registrar, v, chave, sha, pasta)
    return 0


def reproduzir(args: list[str]) -> int:
    exe = _opcao(args, "--exe")
    validar = bool(_opcao(args, "--validar", valor=False))
    extra = dict(a.split("=", 1) for a in args[1:] if "=" in a)
    if not args:
        sys.exit(__doc__)
    s = Sessao(args[0])
    r = offline.reproduzir(s, Path(exe) if exe else None, extra, validar)
    texto = offline.md(r)
    _gravar(s.pasta, {"reproducao.md": texto, "reproducao.json": json.dumps(r, ensure_ascii=False, indent=1)})
    print(texto, end="")
    return 0 if r["estado"] in ("TUDO_CORRIGIDO", "RESTAM_FALHAS") else 1


def ruido(args: list[str]) -> int:
    destino = _opcao(args, "--gravar")
    if not args:
        sys.exit(__doc__)
    dados = Sessao(args[0]).resumo.get("ab")
    if not dados:
        print("esta sessão não tem A/B (SPARK_AB)")
        return 1
    r = estat.ruido(dados)
    if destino:
        Path(destino).write_text(json.dumps(r, indent=1), encoding="utf-8")
    print(json.dumps(r))
    return 0


def consultar(args: list[str]) -> int:
    if not args:
        sys.exit(__doc__)
    caminho, nome = Path(args[0]), args[1] if len(args) > 1 else None
    if not caminho.exists():
        print("nenhuma decisão registrada")
        return 0
    for linha in caminho.read_text(encoding="utf-8").splitlines():
        d = json.loads(linha)
        if nome is None or nome in d.get("chave", ""):
            print(f"{d['data']} {d['chave']} {d.get('sha', '')[:8]} {d['estado']} {d.get('efeito_pct')} % "
                  f"{d.get('ic95_pct')} {d.get('unidades')}")
    return 0


COMANDOS = {"diagnosticar": diagnosticar, "plano": planejar, "ab": ab, "comparar": comparar,
            "reproduzir": reproduzir, "ruido": ruido, "consultar": consultar}


def main(args: list[str] | None = None) -> int:
    args = list(sys.argv[1:] if args is None else args)
    if not args or args[0] not in COMANDOS:
        print(__doc__)
        return 2
    try:
        return COMANDOS[args[0]](args[1:])
    except FileNotFoundError as e:
        print(f"motor: {e}", file=sys.stderr)
        return 1
