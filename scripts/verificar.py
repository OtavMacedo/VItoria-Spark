"""Regras do VItoria Spark verificadas em cada commit (hook) e em cada PR (CI).

Uso:
  python scripts/verificar.py --staged            # pre-commit: linhas adicionadas no índice
  python scripts/verificar.py --base origin/main  # CI: linhas adicionadas desde a base
  python scripts/verificar.py --mensagem <arq>    # commit-msg: a mensagem do commit

Sai com código 1 e lista cada violação (arquivo:linha: regra) quando alguma regra falha.
As regras estão descritas em CONTRIBUTING.md.
"""

import hashlib
import re
import subprocess
import sys

# Termos proibidos em qualquer linha adicionada (nomes de títulos e do seu conteúdo), guardados como
# os 16 primeiros dígitos do SHA-256 da palavra em minúsculas, para que a própria lista não os publique.
PROIBIDAS = {
    "77f17d47f27689dd", "776581edab473928", "861a59ab8affe8eb", "4361cc01e665e9d5",
    "2daf5a7240150afc", "0f47dd87cde8f88e", "b6d5ee9d665b412e", "6326e0e8cfdaab9a",
    "06b9a6eacd7a77b9", "0e1f6dec24570014", "5fb931380e5027a6", "d6b30a45fd16a90d",
    "ceb00a91525da64f", "04f88e0a70fd088d", "2dd3fcf880092df3", "77bf107ebe95b2ed",
    "cf7d21665b5498c0", "7626518e064a333c",
}
PALAVRA = re.compile(r"[a-z0-9_\-]+")

# Dados pessoais e da máquina.
CAMINHO_PESSOAL = re.compile(r"(?i)\b[a-z]:[\\/]+(users|downloads|documents|desktop)\b|/home/[a-z]|/Users/[A-Za-z]")
EMAIL = re.compile(r"[A-Za-z0-9._%+\-]+@[A-Za-z0-9.\-]+\.[A-Za-z]{2,}")
EMAIL_OK = re.compile(r"(?i)(noreply|no-reply)@|@users\.noreply\.github\.com|@example\.(com|org)|\.invalid$")

# Arquivos cujos testes precisam de caminhos pessoais fictícios (o anonimizador dos logs prova que os apaga).
ISENTOS_CAMINHO = {"crates/victory-core/src/session/anon.rs"}

# Mensagens de commit: sem trailers de coautoria e sem menção a ferramentas geradoras.
MSG_PROIBIDA = re.compile(
    r"(?im)^\s*co-authored-by:|generated (with|by)|\b(claude|anthropic|chatgpt|openai|copilot|moon ai|realmoon)\b"
)

# Comentários em código Rust.
COMENTARIO = re.compile(r"^\s*//")
CODIGO_COMENTADO = re.compile(r"^\s*//\s*(let |fn |pub |use |if |for |while |match |return |[a-z_]+\(.*\);\s*$|\}\s*$)")
PENDENCIA = re.compile(r"\b(TODO|FIXME|XXX|HACK)\b")
PENDENCIA_OK = re.compile(r"\b(TODO|FIXME)\(#\d+\)")
ACENTO = re.compile(r"[áàâãéêíóôõúüçÁÀÂÃÉÊÍÓÔÕÚÇ]")


def linhas_adicionadas(args):
    """(arquivo, número da linha, texto) de cada linha adicionada no diff pedido."""
    diff = subprocess.run(
        ["git", "diff", "--unified=0", "--no-color", "--no-ext-diff", *args],
        capture_output=True, text=True, encoding="utf-8", errors="replace", check=True,
    ).stdout
    arquivo, numero = None, 0
    for linha in diff.splitlines():
        if linha.startswith("+++ "):
            arquivo = linha[6:] if linha.startswith("+++ b/") else None
        elif linha.startswith("@@"):
            numero = int(re.search(r"\+(\d+)", linha).group(1))
        elif linha.startswith("+") and arquivo:
            yield arquivo, numero, linha[1:]
            numero += 1


def palavras_proibidas(texto):
    return [p for p in PALAVRA.findall(texto.lower())
            if hashlib.sha256(p.encode()).hexdigest()[:16] in PROIBIDAS]


def verificar_linhas(args):
    erros = []
    for arquivo, n, texto in linhas_adicionadas(args):
        onde = f"{arquivo}:{n}"
        if arquivo == "scripts/verificar.py":
            continue
        if palavras_proibidas(texto):
            erros.append(f"{onde}: nome de título ou do seu conteúdo (regra 1)")
        if CAMINHO_PESSOAL.search(texto) and arquivo not in ISENTOS_CAMINHO:
            erros.append(f"{onde}: caminho pessoal ou da máquina (regra 2)")
        for email in EMAIL.findall(texto):
            if not EMAIL_OK.search(email):
                erros.append(f"{onde}: e-mail pessoal (regra 2)")
        if arquivo.endswith(".rs") and COMENTARIO.match(texto):
            if CODIGO_COMENTADO.match(texto):
                erros.append(f"{onde}: código comentado (regra 5)")
            if PENDENCIA.search(texto) and not PENDENCIA_OK.search(texto):
                erros.append(f"{onde}: TODO/FIXME sem issue, use TODO(#123) (regra 5)")
            if ACENTO.search(texto):
                erros.append(f"{onde}: comentário de código fora do inglês (regra 5)")
    return erros


def verificar_mensagem(caminho):
    texto = open(caminho, encoding="utf-8", errors="replace").read()
    texto = "\n".join(l for l in texto.splitlines() if not l.startswith("#"))
    erros = []
    if MSG_PROIBIDA.search(texto):
        erros.append("mensagem: trailer de coautoria ou menção a ferramenta geradora (regra 4)")
    if palavras_proibidas(texto):
        erros.append("mensagem: nome de título ou do seu conteúdo (regra 1)")
    if CAMINHO_PESSOAL.search(texto):
        erros.append("mensagem: caminho pessoal (regra 2)")
    primeira = texto.strip().splitlines()[0] if texto.strip() else ""
    if len(primeira) > 100:
        erros.append("mensagem: primeira linha com mais de 100 caracteres (regra 4)")
    return erros


def main():
    a = sys.argv[1:]
    if a[:1] == ["--staged"]:
        erros = verificar_linhas(["--cached"])
    elif a[:1] == ["--base"] and len(a) == 2:
        erros = verificar_linhas([f"{a[1]}...HEAD"])
    elif a[:1] == ["--mensagem"] and len(a) == 2:
        erros = verificar_mensagem(a[1])
    else:
        print(__doc__)
        return 2
    for e in erros:
        print(e)
    if erros:
        print(f"\n{len(erros)} violação(ões). Regras em CONTRIBUTING.md.")
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
