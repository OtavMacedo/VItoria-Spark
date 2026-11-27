"""Coordenação entre pessoas e agentes que usam o AutoPilot ao mesmo tempo, pelas issues do GitHub.

Quem começa a trabalhar num alvo o assume numa issue com o rótulo `em-andamento` e uma marca no corpo
(`<!-- spark-alvo: <alvo> | sessao: <id> -->`); o andamento vai em comentários de status, que servem também de
sinal de vida. Outro AutoPilot lê as issues antes de escolher o alvo e pula os que estão em andamento. Um alvo sem
status há mais de `VALIDADE_H` horas fica livre para quem quiser retomá-lo, com um comentário dizendo isso.

O alvo é o `id` de um problema do diagnóstico (`fase:copy_back`, `pipelines_recusadas`...) ou um mais fino
(`pipeline:02384a60fda6f2d0`). Só números agregados vão para as issues (docs/asset-policy.md).

O título diz, em português simples, o que está sendo feito e quanto pesa, sem abrir a issue:
`[AutoPilot: em andamento] Reduzir o tempo de cópia de volta (77 ms por quadro, 31 %)`. Nada de nome interno (`fase:...`,
`pipelines_recusadas`): o alvo técnico fica na marca do corpo. A etapa muda com o trabalho (`em andamento`,
`testando`, `PR aberto`, `resolvido`, `livre`), então `gh issue list` já mostra quem faz o quê e onde está.

Para a IA de outra pessoa, o corpo traz uma ficha técnica em JSON (bloco ```spark-ficha```): alvo, componente,
métrica, evidência agregada, arquivos de código, chaves a testar, hashes verificáveis sem o título e a sugestão do
motor. `coordenar listar --json` a devolve junto de cada issue, sem abrir nenhuma.
"""

import json
import os
import re
import subprocess
import sys
import time
from datetime import datetime, timezone

ROTULO = "em-andamento"
TITULO_MAX = 100
RESUMO_MAX = 85
ETAPAS = {"a/b": "testando", "ab": "testando", "teste": "testando"}
# O que se faz em cada tipo de problema do diagnóstico, dito para quem não conhece o código.
ACOES = {
    "pipelines_recusadas": "Fazer a placa aceitar shaders recusados",
    "descartes": "Recuperar desenhos descartados",
    "memoria": "Evitar estouro da memória da placa",
    "quadros_longos": "Reduzir travadas de mais de 1 s",
    "imagem": "Corrigir defeitos de imagem",
    "falha": "Corrigir um crash",
    "dispositivo_perdido": "Corrigir a perda da placa (crash)",
    "sem_mundo": "Fazer o título chegar ao mundo",
}
JARGAO = re.compile(r"\b[a-z0-9]+_[a-z0-9_]+\b|\b[a-z]+:[a-z_]+\b")
ETAPA = re.compile(r"^\[[^\]]*\]\s*")
VALIDADE_H = 24.0
FICHA = re.compile(r"```spark-ficha\s*\n(?P<json>.*?)\n```", re.S)
MARCA = re.compile(r"<!--\s*spark-alvo:\s*(?P<alvo>[^|>]+?)\s*\|\s*sessao:\s*(?P<sessao>[^>]+?)\s*-->")


def etapa_simples(etapa: str) -> str:
    return ETAPAS.get(etapa.strip().lower(), etapa.strip())


def titulo(etapa: str, resumo: str) -> str:
    """`[AutoPilot: etapa] resumo`, cortado no limite de um título que se lê inteiro numa lista."""
    t = f"[AutoPilot: {etapa_simples(etapa)}] {' '.join(resumo.split())}"
    return t if len(t) <= TITULO_MAX else t[:TITULO_MAX - 1].rstrip() + "…"


def trocar_etapa(atual: str, etapa: str) -> str:
    return f"[AutoPilot: {etapa_simples(etapa)}] {ETAPA.sub('', atual, count=1)}"


def resumo_do_problema(p: dict) -> str:
    """O que se faz no problema do diagnóstico e quanto ele pesa, em português simples."""
    if p["id"].startswith("fase:") and p.get("custo_ms") is not None:
        acao = f"Reduzir o tempo de {p['titulo'][0].lower()}{p['titulo'][1:]}"
        peso = f"{p['custo_ms']:.0f} ms por quadro"
        if p.get("parte_pct") is not None:
            peso += f", {p['parte_pct']:.0f} %"
        return f"{acao} ({peso})"
    acao = ACOES.get(p["id"], p["titulo"])
    if p["id"] == "pipelines_recusadas" and p.get("itens"):
        n = len(p["itens"])
        return f"{acao} ({n} {'recusado' if n == 1 else 'recusados'}: efeito faltando na tela)"
    return acao


def validar_resumo(resumo: str) -> str | None:
    """Um motivo para recusar o resumo, ou None. Pontual: curto, sem caminho nem texto de log."""
    if not resumo.strip():
        return "o resumo do título está vazio"
    if len(resumo) > RESUMO_MAX:
        return f"o resumo tem {len(resumo)} caracteres; no máximo {RESUMO_MAX}, direto ao ponto"
    if JARGAO.search(resumo):
        return ("o resumo tem um nome interno (como `fase:...` ou `nome_com_traco`); diga em português simples o "
                "que está sendo feito e quanto pesa")
    if re.search(r"[A-Za-z]:\\|/home/|/Users/|\\Users\\", resumo):
        return "o resumo tem um caminho de arquivo (docs/asset-policy.md)"
    return None


def ficha(alvo: str, p: dict | None) -> dict:
    """A ficha técnica de um alvo, a partir do problema do diagnóstico (`p`) quando houver: só agregados."""
    f = {"alvo": alvo}
    if not p:
        return f
    f["problema"] = p.get("titulo")
    if p.get("custo_ms") is not None:
        f["metrica"] = {"custo_ms_por_quadro": p["custo_ms"], "parte_do_quadro_pct": p.get("parte_pct")}
    for chave_p, chave_f in (("evidencia", "evidencia"), ("codigo", "codigo"), ("offline", "hashes_sem_o_titulo"),
                             ("sugestao", "sugestao")):
        if p.get(chave_p):
            f[chave_f] = p[chave_p]
    if p.get("chaves"):
        f["chaves"] = [c["nome"] if isinstance(c, dict) else c for c in p["chaves"]]
    return f


def corpo_da_issue(alvo: str, sessao: str, f: dict) -> str:
    import json as _json
    return (f"{marca(alvo, sessao)}\n**Alvo técnico:** `{alvo}`\n\n"
            f"Em andamento por uma sessão de AutoPilot. O andamento fica nos comentários de status; sem status por "
            f"{VALIDADE_H:.0f} h, o alvo fica livre para outra sessão.\n\n"
            "<details><summary>Ficha técnica (para agentes)</summary>\n\n"
            f"```spark-ficha\n{_json.dumps(f, ensure_ascii=False, indent=1)}\n```\n</details>\n")


def ler_ficha(corpo: str) -> dict | None:
    m = FICHA.search(corpo or "")
    if not m:
        return None
    try:
        return json.loads(m["json"])
    except ValueError:
        return None


def marca(alvo: str, sessao: str) -> str:
    return f"<!-- spark-alvo: {alvo} | sessao: {sessao} -->"


def sessao_atual() -> str:
    """O id desta sessão de AutoPilot: `SPARK_SESSAO`, ou máquina e hora de início (vários agentes podem usar a
    mesma conta do GitHub, então a conta não identifica quem trabalha)."""
    return os.environ.get("SPARK_SESSAO") or f"{os.environ.get('COMPUTERNAME', 'maquina')}-{os.getpid()}"


def _horas(iso: str, agora: float) -> float:
    quando = datetime.fromisoformat(iso.replace("Z", "+00:00")).astimezone(timezone.utc).timestamp()
    return (agora - quando) / 3600


def reivindicacoes(issues: list[dict], agora: float | None = None, validade_h: float = VALIDADE_H) -> list[dict]:
    """As issues abertas que assumem um alvo, das mais antigas para as mais novas, com `ativa` (status recente)."""
    agora = agora or time.time()
    saida = []
    for i in issues:
        m = MARCA.search(i.get("body") or "")
        if not m or i.get("state", "OPEN").upper() != "OPEN":
            continue
        rotulos = {r["name"] if isinstance(r, dict) else r for r in i.get("labels", [])}
        idade = _horas(i["updatedAt"], agora)
        saida.append({"numero": i["number"], "titulo": i.get("title", ""), "alvo": m["alvo"].strip(),
                      "sessao": m["sessao"].strip(), "horas_sem_status": round(idade, 1),
                      "ativa": ROTULO in rotulos and idade < validade_h, "ficha": ler_ficha(i.get("body") or "")})
    return sorted(saida, key=lambda r: r["numero"])


def dono(alvo: str, reivs: list[dict]) -> dict | None:
    """A reivindicação ativa mais antiga do alvo: é ela que vale quando duas sessões assumem ao mesmo tempo."""
    return next((r for r in reivs if r["alvo"] == alvo and r["ativa"]), None)


def decidir(alvo: str, sessao: str, reivs: list[dict]) -> dict:
    """O que fazer ao querer o alvo: `livre` (abrir), `minha` (seguir), `ocupado` (escolher outro) ou `retomar`
    (a issue parada passa para esta sessão)."""
    atual = dono(alvo, reivs)
    if atual:
        return {"acao": "minha" if atual["sessao"] == sessao else "ocupado", "issue": atual["numero"]}
    parada = next((r for r in reivs if r["alvo"] == alvo), None)
    if parada:
        return {"acao": "retomar", "issue": parada["numero"]}
    return {"acao": "livre", "issue": None}


def anotar(problemas: list[dict], reivs: list[dict], sessao: str) -> list[dict]:
    """Marca os problemas do diagnóstico que outra sessão já trabalha (`em_andamento`: número da issue)."""
    for p in problemas:
        atual = dono(p["id"], reivs)
        if atual and atual["sessao"] != sessao:
            p["em_andamento"] = atual["numero"]
    return problemas


# --- GitHub (gh) ---

def _gh(*args: str, entrada: str | None = None) -> str:
    r = subprocess.run(["gh", *args], input=entrada, capture_output=True, text=True, encoding="utf-8")
    if r.returncode != 0:
        sys.exit(f"gh {' '.join(args[:2])}: {r.stderr.strip()}")
    return r.stdout


def ler_issues() -> list[dict]:
    texto = _gh("issue", "list", "--state", "open", "--label", "autopilot", "--limit", "200",
                "--json", "number,title,body,labels,updatedAt,state")
    return json.loads(texto or "[]")


def _garantir_rotulo() -> None:
    subprocess.run(["gh", "label", "create", ROTULO, "--color", "FB8C00", "--force",
                    "--description", "Alguém (pessoa ou AutoPilot) trabalha nisto agora; veja o último status"],
                   capture_output=True)


def _comentar(numero: int, texto: str) -> None:
    _gh("issue", "comment", str(numero), "--body-file", "-", entrada=texto)


def _carimbo() -> str:
    return datetime.now(timezone.utc).strftime("%Y-%m-%d %H:%M UTC")


def assumir(alvo: str, resumo: str, sessao: str, rotulos: list[str], problema: dict | None = None) -> int:
    if motivo := validar_resumo(resumo):
        sys.exit(f"coordenar assumir: {motivo}")
    reivs = reivindicacoes(ler_issues())
    d = decidir(alvo, sessao, reivs)
    if d["acao"] == "ocupado":
        print(f"ocupado: o alvo `{alvo}` está em andamento em #{d['issue']}; escolha outro alvo")
        return 1
    if d["acao"] == "minha":
        print(f"minha: o alvo `{alvo}` já é desta sessão em #{d['issue']}")
        return 0
    _garantir_rotulo()
    if d["acao"] == "retomar":
        n = d["issue"]
        corpo = _gh("issue", "view", str(n), "--json", "body", "-q", ".body")
        _gh("issue", "edit", str(n), "--body-file", "-", "--add-label", ROTULO,
            "--title", titulo("em andamento", resumo), entrada=MARCA.sub(marca(alvo, sessao), corpo))
        _comentar(n, f"**[AutoPilot Status]** ({_carimbo()}): retomado por outra sessão; o último status tinha mais de "
                     f"{VALIDADE_H:.0f} h.")
        print(f"retomado: #{n}")
        return 0
    corpo = corpo_da_issue(alvo, sessao, ficha(alvo, problema))
    url = _gh("issue", "create", "--title", titulo("em andamento", resumo), "--body-file", "-", "--label",
              ",".join(["autopilot", ROTULO, *rotulos]), entrada=corpo).strip()
    n = int(url.rstrip("/").rsplit("/", 1)[-1])
    # Duas sessões podem abrir ao mesmo tempo: vale a issue mais antiga, e a outra se fecha como duplicada.
    atual = dono(alvo, reivindicacoes(ler_issues()))
    if atual and atual["numero"] != n:
        _comentar(n, f"Duplicada de #{atual['numero']}, aberta ao mesmo tempo por outra sessão.")
        _gh("issue", "close", str(n), "--reason", "not planned")
        print(f"ocupado: outra sessão assumiu `{alvo}` em #{atual['numero']} ao mesmo tempo; escolha outro alvo")
        return 1
    print(f"assumido: #{n} ({url})")
    return 0


def _etapa(numero: int, etapa: str) -> None:
    atual = _gh("issue", "view", str(numero), "--json", "title", "-q", ".title").strip()
    novo = trocar_etapa(atual, etapa)
    if novo != atual:
        _gh("issue", "edit", str(numero), "--title", novo)


def status(numero: int, texto: str, etapa: str | None = None) -> int:
    _comentar(numero, f"**[AutoPilot Status]** ({_carimbo()}): {texto}")
    if etapa:
        _etapa(numero, etapa)
    return 0


def liberar(numero: int, motivo: str) -> int:
    _comentar(numero, f"**[AutoPilot Status]** ({_carimbo()}): liberado. {motivo}".strip())
    _gh("issue", "edit", str(numero), "--remove-label", ROTULO)
    _etapa(numero, "livre")
    return 0


ESCRITA = {"ADMIN", "MAINTAIN", "WRITE"}


def modo_de_entrega(permissao: str) -> str:
    """`direta` para quem pode escrever no repositório (commit e push na main: o A/B do motor e as checagens locais
    já provaram a mudança); `pr` para os demais (a revisão de quem mantém o projeto é a prova que falta)."""
    return "direta" if permissao.upper() in ESCRITA else "pr"


def _git(*args: str) -> str:
    r = subprocess.run(["git", *args], capture_output=True, text=True, encoding="utf-8")
    if r.returncode != 0:
        sys.exit(f"git {' '.join(args[:2])}: {r.stderr.strip()}")
    return r.stdout.strip()


def _remoto(repo: str) -> str:
    """O remote do git que aponta para `repo` (`dono/nome`), qualquer que seja o nome dele."""
    for linha in _git("remote", "-v").splitlines():
        nome, url = linha.split()[:2]
        if repo.lower() in url.lower().removesuffix(".git"):
            return nome
    sys.exit(f"nenhum remote aponta para {repo}")


def entregar(numero: int, como_pr: bool) -> int:
    """Leva o commit atual (a mudança provada) para o projeto e fecha o ciclo da issue `numero`."""
    info = json.loads(_gh("repo", "view", "--json", "nameWithOwner,viewerPermission,defaultBranchRef"))
    modo = "pr" if como_pr else modo_de_entrega(info.get("viewerPermission") or "")
    principal = (info.get("defaultBranchRef") or {}).get("name") or "main"
    remoto = _remoto(info["nameWithOwner"])
    if modo == "direta":
        _git("fetch", remoto, principal)
        _git("rebase", f"{remoto}/{principal}")
        regras = subprocess.run([sys.executable, "scripts/verificar.py", "--base", f"{remoto}/{principal}"])
        if regras.returncode != 0:
            sys.exit("as regras do projeto recusaram os commits; nada foi enviado")
        _git("push", remoto, f"HEAD:{principal}")
        sha = _git("rev-parse", "--short", "HEAD")
        status(numero, f"entregue direto na `{principal}` em {sha}.", "resolvido")
        _gh("issue", "edit", str(numero), "--remove-label", ROTULO)
        _gh("issue", "close", str(numero), "--reason", "completed")
        print(f"entregue: {sha} na {principal}; #{numero} fechada")
        return 0
    ramo = _git("branch", "--show-current")
    if ramo in ("", principal):
        sys.exit("para um PR, a mudança precisa estar num branch próprio (não na principal)")
    _git("push", "-u", remoto, ramo)
    url = _gh("pr", "create", "--fill", "--body", f"Fecha #{numero}", "--head", ramo).strip()
    status(numero, f"PR aberto: {url}", "PR aberto")
    print(f"PR aberto: {url}")
    return 0


def listar(como_json: bool) -> int:
    reivs = reivindicacoes(ler_issues())
    if como_json:
        print(json.dumps(reivs, ensure_ascii=False, indent=1))
        return 0
    if not reivs:
        print("nenhum alvo em andamento")
    for r in reivs:
        estado = "em andamento" if r["ativa"] else "parado (livre para retomar)"
        print(f"#{r['numero']} {r['titulo']}  [{r['alvo']}] ({estado}, último status há "
              f"{r['horas_sem_status']} h, sessão {r['sessao']})")
    return 0


USO = """Uso:
  python scripts/motor.py coordenar listar [--json]
  python scripts/motor.py coordenar assumir <alvo> (--de <sessão> | --resumo "<o que se faz (quanto pesa)>")
                                                    [--rotulo nvidia ...]
      Título: `[AutoPilot: em andamento] <resumo>`, em português simples (até 85 letras, sem nomes internos); com `--de`, o
      resumo sai do diagnóstico da execução. Ex.: "Reduzir o tempo de cópia de volta (77 ms por quadro, 31 %)".
  python scripts/motor.py coordenar status <issue> "<o que foi feito e o próximo passo>" [--etapa "A/B"]
      `--etapa` troca o começo do título (`testando`, `PR aberto`, `resolvido`...).
  python scripts/motor.py coordenar liberar <issue> ["<motivo>"]
  python scripts/motor.py coordenar entregar <issue> [--pr]
      Quem pode escrever no repositório entrega direto: rebase, regras do projeto, push na principal, issue fechada.
      Quem não pode (ou passa `--pr`) abre um PR do branch atual com `Fecha #<issue>`.
A sessão é `SPARK_SESSAO` (defina uma por agente) ou máquina e processo."""


def comando(args: list[str]) -> int:
    if not args:
        sys.exit(USO)
    sub, resto = args[0], args[1:]
    if sub == "listar":
        return listar("--json" in resto)
    if sub == "assumir" and resto and ("--resumo" in resto or "--de" in resto):
        alvo = resto[0]
        rotulos = [resto[j + 1] for j, a in enumerate(resto) if a == "--rotulo" and j + 1 < len(resto)]
        p = None
        if "--resumo" in resto:
            resumo = resto[resto.index("--resumo") + 1]
        else:
            from . import problemas
            from .sessao import Sessao
            d = problemas.diagnosticar(Sessao(resto[resto.index("--de") + 1]))
            p = next((p for p in d["problemas"] if p["id"] == alvo.split("|")[0]), None)
            if not p:
                sys.exit(f"coordenar assumir: o diagnóstico não tem o problema `{alvo}`; use --resumo")
            resumo = resumo_do_problema(p)
        return assumir(alvo, resumo, sessao_atual(), rotulos, p)
    if sub == "status" and len(resto) >= 2:
        etapa = None
        if "--etapa" in resto:
            i = resto.index("--etapa")
            etapa = resto[i + 1]
            del resto[i:i + 2]
        return status(int(resto[0].lstrip("#")), " ".join(resto[1:]), etapa)
    if sub == "entregar" and resto:
        return entregar(int(resto[0].lstrip("#")), "--pr" in resto)
    if sub == "liberar" and resto:
        return liberar(int(resto[0].lstrip("#")), " ".join(resto[1:]))
    sys.exit(USO)
