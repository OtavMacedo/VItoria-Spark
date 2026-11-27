"""O motor de diagnóstico do VItoria Spark: uma execução, todos os problemas, a próxima execução planejada.

Uso:
  python scripts/motor.py diagnosticar <sessão> [--json] [--gravar] [--coordenar]
      Todos os problemas da execução em ordem de custo, com evidência, correção sugerida, chaves a testar e código;
      os problemas de medição primeiro. Com `--gravar`: `diagnostico.md`, `diagnostico.json`, `plano.toml` e
      `plano.md` na pasta da sessão.
  python scripts/motor.py plano <sessão> [--periodo 20] [--gravar]
      Só o plano: o `SPARK_PLAN` que testa de uma vez todas as chaves candidatas (fatorial), as chaves que pedem
      execução própria e os itens verificáveis sem o título. Com `--gravar`: `plano.toml` e `plano.md`.
  python scripts/motor.py plano --juntar <plano.toml> [...] [--saida <plano.toml>] [--periodo N]
      Carona: junta os experimentos de vários planos (de várias pessoas ou agentes, no mesmo executável) em
      fatoriais de até 5 fatores, para uma execução responder a todos; diz em que `--exp` e fator ficou cada um.
  python scripts/motor.py fila status | fila rodar <nome> [--classe pesado|jogo] [--modo exclusiva|intercalada]
                                                          -- <comando...>
      A fila da máquina (placa e CPU): builds rodam junto de execuções com A/B dentro delas, nunca junto das
      que medem números absolutos (`motor/fila.py`).
  python scripts/motor.py ab <sessão> [--exp N] [--imagem <json>] [--ruido <ruido.json>] [--gravar]
                                      [--registrar <decisoes.jsonl> [--chave <NOME>] [--sha <commit>]]
      O veredito do A/B de uma execução (`SPARK_AB`; N de `SPARK_PLAN`). Num fatorial, um veredito por fator.
  python scripts/motor.py comparar --a <sessão> [...] --b <sessão> [...] [--imagem <json>] [--sem-medicao]
                                      [--gravar] [--registrar <decisoes.jsonl> --chave <NOME> [--sha <commit>]]
      O veredito de B contra A com execuções inteiras (2+ de cada lado) e as fases que mais mudaram.
  python scripts/motor.py reproduzir <sessão> [--exe <victory.exe>] [--validar] [--gravar] [CHAVE=valor ...]
      Sem o título: retraduz os shaders que a execução guardou (`SPARK_OFFLINE=1`) e cria as pipelines na placa;
      diz quais falhas o build atual corrigiu. Com `--gravar`, `reproducao.json` lembra o que já falhou aqui fora
      (só uma recusa que se repetiu sem o título pode sair CORRIGIDO sem ele).
  python scripts/motor.py ruido <sessão A/A> --gravar <ruido.json>
      O piso de ruído da máquina, de uma execução com os dois braços iguais.
  python scripts/motor.py consultar <decisoes.jsonl> [<NOME>]
      O que já foi decidido (antes de testar de novo).
  python scripts/motor.py coordenar listar | assumir <alvo> --titulo "..." | status <issue> "..." | liberar <issue>
      Quem trabalha em quê, pelas issues do GitHub (rótulo `em-andamento`): assumir antes de começar, um status a
      cada passo, liberar ao desistir; o PR que resolve fecha a issue (`motor/coordenacao.py`).

<sessão> = a pasta de uma execução (vale a mais nova `resumo.json` dentro dela) ou o próprio `resumo.json`.
Ler não grava: sem `--gravar` os comandos só imprimem, e a pasta de uma execução (a sua ou a de outra pessoa) fica
como estava. O que `--gravar` grava fica na pasta da sessão, nunca dentro do repositório (docs/asset-policy.md).
"""

import json
import sys
import time
import tomllib
from pathlib import Path

from . import coordenacao, estat, fila, offline, plano, problemas, relatorio, veredito
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


def _gravar(pasta: Path, arquivos: dict[str, str], pedido: bool) -> None:
    """Grava os arquivos na pasta da sessão, se foi pedido (`--gravar`); nunca dentro do repositório."""
    if not pedido:
        return
    if dentro_do_repo(pasta):
        print("(nada gravado: a sessão está dentro do repositório; docs/asset-policy.md)", file=sys.stderr)
        return
    for nome, texto in arquivos.items():
        if texto:
            (pasta / nome).write_text(texto, encoding="utf-8")


def _fora_do_repo(caminho: str) -> None:
    """Sai com erro se `caminho` (um arquivo a gravar) fica dentro do repositório."""
    if dentro_do_repo(Path(caminho).resolve().parent):
        sys.exit(f"{caminho}: saída de execução não se grava dentro do repositório (docs/asset-policy.md)")


def _registrar(caminho: str | None, v: dict, chave: str | None, sha: str | None, pasta: Path) -> None:
    if not caminho:
        return
    _fora_do_repo(caminho)
    with open(caminho, "a", encoding="utf-8") as f:
        for linha in veredito.registros(v, chave, sha, str(pasta), time.strftime("%Y-%m-%d %H:%M")):
            f.write(json.dumps(linha, ensure_ascii=False) + "\n")


def diagnosticar(args: list[str]) -> int:
    so_json = _opcao(args, "--json", valor=False)
    gravar = bool(_opcao(args, "--gravar", valor=False))
    periodo = int(_opcao(args, "--periodo") or 20)
    coordenar = bool(_opcao(args, "--coordenar", valor=False))
    if not args:
        sys.exit(__doc__)
    s = Sessao(args[0])
    d = problemas.diagnosticar(s)
    if coordenar:
        reivs = coordenacao.reivindicacoes(coordenacao.ler_issues())
        coordenacao.anotar(d["problemas"], reivs, coordenacao.sessao_atual())
    p = plano.montar(d, periodo)
    d["plano"] = p
    arquivo_plano = s.pasta / "plano.toml"
    texto = relatorio.diagnostico(d, p, gravado=gravar)
    _gravar(s.pasta, {"diagnostico.md": texto, "diagnostico.json": json.dumps(d, ensure_ascii=False, indent=1),
                      "plano.toml": plano.toml(p), "plano.md": plano.md(p, str(arquivo_plano))}, gravar)
    print(json.dumps(d, ensure_ascii=False) if so_json else texto, end="" if not so_json else "\n")
    ocupados = [p for p in d["problemas"] if p.get("em_andamento")]
    if ocupados and not so_json:
        print("\nEm andamento por outra sessão (escolha outro alvo): "
              + ", ".join(f"`{p['id']}` (#{p['em_andamento']})" for p in ocupados))
    return 0


def juntar(args: list[str]) -> int:
    """`plano --juntar <plano.toml> [...] [--saida <plano.toml>] [--periodo N]`."""
    periodo = _opcao(args, "--periodo")
    saida = _opcao(args, "--saida")
    if not args:
        sys.exit(__doc__)
    pedidos = [{"origem": Path(a).stem, **tomllib.loads(Path(a).read_text(encoding="utf-8-sig"))} for a in args]
    junto = plano.juntar(pedidos, int(periodo) if periodo else None)
    texto = plano.toml(junto)
    print(plano.md_da_carona(junto), end="")
    if saida:
        _fora_do_repo(saida)
        Path(saida).write_text(texto, encoding="utf-8")
        print(f"\nRodar: `python scripts/autopilot.py rodar carona-1 SPARK_PLAN={saida}`")
    else:
        print("\nO plano (`--saida <arquivo>` o grava):\n\n" + texto, end="")
    return 0


def planejar(args: list[str]) -> int:
    if _opcao(args, "--juntar", valor=False):
        return juntar(args)
    periodo = int(_opcao(args, "--periodo") or 20)
    gravar = bool(_opcao(args, "--gravar", valor=False))
    if not args:
        sys.exit(__doc__)
    s = Sessao(args[0])
    p = plano.montar(problemas.diagnosticar(s), periodo)
    arquivo = s.pasta / "plano.toml"
    texto = plano.md(p, str(arquivo) if gravar else None)
    _gravar(s.pasta, {"plano.toml": plano.toml(p), "plano.md": texto}, gravar)
    print(texto, end="")
    if not gravar and plano.toml(p):
        print("\nO `plano.toml` (com `--gravar` ele fica na pasta da sessão):\n\n" + plano.toml(p), end="")
    return 0


def ab(args: list[str]) -> int:
    exp = _opcao(args, "--exp")
    imagem, ruido = _json(_opcao(args, "--imagem")), _json(_opcao(args, "--ruido"))
    registrar, chave, sha = (_opcao(args, n) for n in ("--registrar", "--chave", "--sha"))
    gravar = bool(_opcao(args, "--gravar", valor=False))
    if not args:
        sys.exit(__doc__)
    s = Sessao(args[0])
    v = veredito.ab(s, int(exp) if exp is not None else None, imagem, ruido)
    texto = veredito.md(v)
    sufixo = f"-{exp}" if exp is not None else ""
    _gravar(s.pasta, {f"veredito{sufixo}.md": texto,
                      f"veredito{sufixo}.json": json.dumps(v, ensure_ascii=False, indent=1)}, gravar)
    print(texto, end="")
    _registrar(registrar, v, chave, sha, s.pasta)
    return 0


def comparar(args: list[str]) -> int:
    imagem = _json(_opcao(args, "--imagem"))
    sem_medicao = bool(_opcao(args, "--sem-medicao", valor=False))
    gravar = bool(_opcao(args, "--gravar", valor=False))
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
    _gravar(pasta, {"veredito.md": texto, "veredito.json": json.dumps(v, ensure_ascii=False, indent=1)}, gravar)
    print(texto, end="")
    _registrar(registrar, v, chave, sha, pasta)
    return 0


def reproduzir(args: list[str]) -> int:
    exe = _opcao(args, "--exe")
    validar = bool(_opcao(args, "--validar", valor=False))
    gravar = bool(_opcao(args, "--gravar", valor=False))
    extra = dict(a.split("=", 1) for a in args[1:] if "=" in a)
    if not args:
        sys.exit(__doc__)
    s = Sessao(args[0])
    r = offline.reproduzir(s, Path(exe) if exe else None, extra, validar)
    texto = offline.md(r)
    _gravar(s.pasta, {"reproducao.md": texto, "reproducao.json": json.dumps(r, ensure_ascii=False, indent=1)}, gravar)
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
        _fora_do_repo(destino)
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


COMANDOS = {"fila": fila.comando, "diagnosticar": diagnosticar, "plano": planejar, "ab": ab, "comparar": comparar,
            "reproduzir": reproduzir, "ruido": ruido, "consultar": consultar,
            "coordenar": coordenacao.comando}


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
