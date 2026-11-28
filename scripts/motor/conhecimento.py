"""O que o motor sabe do Spark: de cada fase do quadro, onde fica o código e que chaves a mudam.

É a única tabela a editar quando uma chave nova nasce ou um módulo muda de lugar. As chaves que alternam dentro de
uma execução (`SPARK_AB`) não são repetidas aqui: saem de `crates/victory-gpu/src/switches.rs`, a tabela que o
runtime usa. `scripts/tests/test_motor.py` confere que todo arquivo e toda chave citados existem no repositório.
"""

import re
from pathlib import Path

RAIZ = Path(__file__).resolve().parent.parent.parent
GPU = "crates/victory-gpu/src/"

# A "parte do quadro" de switches.rs e a fase do resumo que ela muda.
FASE_DA_PARTE = {"copias": "copies", "copia_de_volta": "copy_back", "preparo": "preparing", "texturas": "textures",
                 "gpu": "gpu"}
# Chaves alternáveis que não são liga/desliga: os dois valores a comparar.
VALORES_AB = {"VICTORY_TEXTURE_ROOM_PERCENT": "80/94", "VICTORY_WIDE_THREADS": "4/8"}
# Chaves cujo efeito na memória persiste depois que o braço troca (uma escrita perdida ou adiada fica velha até o
# fim da execução): no A/B na execução os outros braços herdam esse estado, então o tempo vale e a imagem por
# braço não; a imagem se julga com execuções separadas.
IMAGEM_SO_ENTRE_EXECUCOES = ("VICTORY_SKIP_WC_BACK", "VICTORY_MERGE_MASK", "VICTORY_WC_NO_BACK")
# O máximo de fatores de um experimento (`victory_core::ab::MAX_FACTORS`).
FATORES_MAX = 5


def _queue2_no_bit_2(env: dict) -> bool:
    try:
        return int(env.get("VICTORY_QUEUE2", "0")) & 2 != 0
    except ValueError:
        return False


# Chave alternável e a configuração que a deixa sem efeito (o runtime recusa o plano; o motor nem a propõe).
INERTE = {"VICTORY_MERGE_MASK": _queue2_no_bit_2}
# Chaves alternáveis já decididas contra, com o motivo: o tempo delas já foi medido e a imagem as recusa, então
# propô-las de novo só gasta um fator de cada plano e, com estado que persiste, estraga as capturas das outras.
REJEITADA = {"VICTORY_SKIP_WC_BACK": "perde escritas da placa e estraga a imagem (README, \"Ainda não\")"}

# Por fase: título, o que é, onde fica o código, e chaves lidas uma vez (precisam de execução própria), cada
# uma com o valor a tentar e o que faz.
FASES = {
    "gpu": {
        "titulo": "Espera pela placa",
        "o_que": "tempo das submissões esperando a GPU terminar; a placa é o limite se ela não ficou ociosa",
        "codigo": [GPU + "host.rs", GPU + "cardmeter.rs", GPU + "pipeline/host.rs"],
        "chaves": [("VICTORY_ASYNC_PRESENT", "1", "apresenta sem esperar a fila do título")],
        "ideia": "ver quais listas custam mais no relógio da placa (cardmeter) e os passes repetidos; reduzir "
                 "resolução ou passes só depois de saber qual pipeline pesa",
    },
    "copies": {
        "titulo": "Cópias de memória do título",
        "o_que": "bytes do título copiados para a placa a cada submissão",
        "codigo": [GPU + "host.rs", GPU + "plaincopy.rs", GPU + "shadow.rs", GPU + "card_mask.rs"],
        "chaves": [("VICTORY_JOIN_COPIES", "1", "junta numa só as cópias que continuam a anterior"),
                   ("VICTORY_QUEUE2", "1", "manda os bytes por uma segunda fila")],
        "ideia": "copiar menos (só as faixas escritas) antes de copiar mais rápido: comparar MB por quadro com o "
                 "que o write watch diz que mudou",
    },
    "write_watch": {
        "titulo": "Consultas ao write watch",
        "o_que": "perguntas ao sistema sobre que páginas o título escreveu",
        "codigo": [GPU + "watch.rs", GPU + "dirty_ranges.rs"],
        "chaves": [("VICTORY_VIEW_KNOWN", "1", "mantém conhecidos os blocos de vista já escritos")],
        "ideia": "menos consultas maiores (uma por reserva) e nenhuma para memória que os guardas já vigiam",
    },
    "textures": {
        "titulo": "Atualização de texturas",
        "o_que": "ler, comparar e converter texturas do título (todas as threads)",
        "codigo": [GPU + "texture.rs", GPU + "heap_worker.rs", GPU + "tiles.rs"],
        "chaves": [("VICTORY_TEXTURE_NO_WAIT", "1", "não espera a memória do título ficar parada"),
                   ("VICTORY_HEAP_WORKER", "1", "texturas escolhidas por índice numa thread própria"),
                   ("VICTORY_PARALLEL_REFRESH", "1", "atualiza texturas em paralelo")],
        "ideia": "evitar reenviar textura que não mudou (ver o aviso de textura reenviada) e tirar a conversão do "
                 "caminho da submissão",
    },
    "copy_back": {
        "titulo": "Cópia de volta",
        "o_que": "o que os shaders escreveram na placa copiado de volta para a memória do título",
        "codigo": [GPU + "host.rs", GPU + "census.rs", GPU + "texback.rs", GPU + "backreads.rs"],
        "chaves": [("VICTORY_TEXTURE_BACK", "1", "traz texturas de volta pelo caminho próprio")],
        "ideia": "trazer de volta só o que o título lê depois (o censo de leituras diz o quê); o resto fica na placa",
    },
    "remap": {
        "titulo": "Remapeamento da memória do título",
        "o_que": "o runtime trocando o que a memória do título mostra (desmapear e mapear vistas); roda nas threads "
                 "do título e disputa a trava com quem lê a memória",
        "codigo": ["crates/victory-hle/src/modules/xmem.rs", GPU + "watch.rs"],
        "chaves": [],
        "ideia": "o custo está nas chamadas de desmapear do sistema: agrupar remapeamentos vizinhos e não desfazer "
                 "vistas que serão mapeadas de novo no mesmo lugar",
    },
    "pipeline_wait": {
        "titulo": "Espera por pipelines",
        "o_que": "desenhos esperando a pipeline do host ser compilada",
        "codigo": [GPU + "pipeline/make.rs", GPU + "pipeline/library.rs", GPU + "shader_cache.rs"],
        "chaves": [("VICTORY_PIPELINE_WAIT_MS", "100", "orçamento de espera por quadro (padrão 500 ms)")],
        "ideia": "aquecer antes de medir (`scripts/autopilot.py aquecer`); se persistir com caches quentes, a "
                 "biblioteca de pipelines não está cobrindo essas pipelines",
    },
    "preparing_other": {
        "titulo": "Preparo das submissões (fora cópias, texturas e write watch)",
        "o_que": "o resto do preparo na CPU: descritores, tabelas, comparações na placa",
        "codigo": [GPU + "host.rs", GPU + "descriptor/host.rs", GPU + "draw.rs", GPU + "heap_pass.rs"],
        "chaves": [("VICTORY_ASYNC_SUBMIT", "1", "prepara numa thread própria"),
                   ("VICTORY_LOOKUP_CACHE", "1", "guarda as buscas repetidas")],
        "ideia": "perfilar o preparo (as linhas periódicas de victory_gpu::host no eventos.jsonl dizem que passe "
                 "pesa)",
    },
}

# Outros problemas: onde fica o código.
CODIGO = {
    "shaders_nao_traduzidos": ["crates/victory-shader/src/gfx10/opcodes.rs", "crates/victory-shader/src/emit/lower.rs",
                               "apps/victory/src/translate.rs"],
    "pipelines_recusadas": [GPU + "pipeline/host.rs", "crates/victory-shader/src/emit/wave.rs",
                            "crates/victory-shader/src/emit/host.rs"],
    "descartes": [GPU + "draw.rs", GPU + "pipeline/make.rs"],
    "memoria": [GPU + "resource.rs", GPU + "shadow.rs", GPU + "memory.rs"],
    "imagem": [GPU + "queue.rs", GPU + "draw.rs", GPU + "descriptor/host.rs"],
    "quadros_longos": [GPU + "framelog.rs", GPU + "pipeline/make.rs", "crates/victory-hle/src/modules/xmem.rs"],
}

# Chaves lidas uma vez citadas fora de FASES.
CHAVES_DE_MEMORIA = [("VICTORY_TEXTURE_BUDGET_MB", "orçamento das texturas na placa"),
                     ("VICTORY_CARD_BUFFERS_MB", "orçamento das cópias na memória da placa")]
CHAVE_DO_CONSTRUTOR = "VICTORY_BVH_BUILDER"
CHAVE_DE_ONDA = "VICTORY_WAVE_WIDE_ONLY"

# Avisos que dizem respeito à imagem: trecho da mensagem e o que significa.
SINAIS_DE_IMAGEM = [
    ("black frame", "quadro preto depois de um que não era"),
    ("changed after its upload", "textura mudou depois de enviada sem aviso (reenviada: custo e risco de imagem velha)"),
    ("left null", "vista deixada nula (o shader lê zeros)"),
    ("not implemented", "método de criação de pipeline não implementado (o título fica sem a pipeline)"),
    ("not drawn", "desenhos não feitos"),
    ("without a host pipeline", "trabalho descartado por falta de pipeline do host"),
    ("does not translate", "shader que não traduz (os desenhos dele somem)"),
    ("unordered views refused", "vistas de acesso não ordenado recusadas"),
]


def alternaveis(raiz: Path = RAIZ) -> dict[str, str]:
    """As chaves que alternam dentro de uma execução e a fase que cada uma muda, lidas da tabela do runtime."""
    texto = (raiz / "crates/victory-gpu/src/switches.rs").read_text(encoding="utf-8")
    nomes = dict(re.findall(r"static (\w+): Toggle =\s*Toggle::\w+\(\s*\"(VICTORY_\w+)\"", texto))
    partes = re.findall(r"toggle: &(\w+),\s*part: \"(\w+)\"", texto)
    return {nomes[ident]: FASE_DA_PARTE.get(parte, parte) for ident, parte in partes if ident in nomes}


def espec_ab(nome: str) -> str:
    """Como a chave entra num `SPARK_AB`: `NOME` (0/1) ou `NOME=a/b`."""
    return f"{nome}={VALORES_AB[nome]}" if nome in VALORES_AB else nome


def chaves_da_fase(fase: str, env: dict, raiz: Path = RAIZ) -> list[dict]:
    """As chaves candidatas de uma fase: as alternáveis (para um A/B na execução) e as lidas uma vez que a
    execução não tinha ligadas (precisam de execução própria)."""
    out = []
    for nome, sua in alternaveis(raiz).items():
        if sua == fase or (fase == "preparing_other" and sua == "preparing"):
            if nome in REJEITADA or (nome in INERTE and INERTE[nome](env)):
                continue
            out.append({"nome": nome, "alternavel": True, "ab": espec_ab(nome), "atual": env.get(nome)})
    for nome, valor, nota in FASES.get(fase, {}).get("chaves", []):
        if env.get(nome) != valor:
            out.append({"nome": nome, "alternavel": False, "valor": valor, "atual": env.get(nome), "nota": nota})
    return out
