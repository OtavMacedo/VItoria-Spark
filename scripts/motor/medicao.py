"""Problemas de medição: quando os números de uma execução não servem para julgar nada.

Uma execução com o driver num estado ruim, com a máquina paginando ou com poucas amostras produz um veredito falso
com a mesma cara de um verdadeiro. Cada verificação aqui devolve um achado com `grave` (os números não valem: o
motor responde PROBLEMA_DE_MEDICAO) ou só um alerta. Os limites vêm de execuções medidas e ficam no topo do arquivo.
"""

from .estat import intervalo_da_mediana
from .sessao import Sessao

# Criação do dispositivo: segundos a partir dos quais o driver está num estado em que a execução não vale
# (o normal é abaixo de 10 s).
DISPOSITIVO_LENTO_S = 30.0
# Cache de shaders do driver: tamanho a partir do qual ele atrasa a criação do dispositivo e pipelines.
CACHE_DO_DRIVER_MB = 4096
# Janelas de 1 s da cena estável abaixo das quais a mediana do quadro não está medida.
JANELAS_MIN = 20
# Falhas de página por segundo num quadro longo, e quantas vezes a média da execução, para ser tempestade.
FALTAS_POR_S = 5000.0
FALTAS_VEZES_A_MEDIA = 3.0
# Parte do tempo no mundo em quadros de mais de 1 s a partir da qual a tempestade invalida a execução.
FALTAS_PARTE_GRAVE_PCT = 10.0
# RAM livre mínima (MB): abaixo de `RAM_GRAVE_MB` o sistema pagina; abaixo de `RAM_ALERTA_MB`, alerta.
RAM_GRAVE_MB = 512
RAM_ALERTA_MB = 1024
# Parte das janelas da cena estável com pipelines ainda sendo feitas a partir da qual os caches estavam frios.
CACHES_FRIOS_PARTE = 0.2
# Meia largura do IC 95 % da mediana (em % dela) acima da qual a cena não estava estável.
IC_LARGO_PCT = 10.0


def _achado(ident: str, grave: bool, texto: str, acao: str) -> dict:
    return {"id": ident, "grave": grave, "texto": texto, "acao": acao}


def verificar(s: Sessao) -> list[dict]:
    """Os problemas de medição da execução, os graves primeiro."""
    r = s.resumo
    out = []
    if r.get("crash"):
        out.append(_achado("falha", True, "a execução terminou numa falha", "corrigir a falha antes de medir"))
    if r.get("device_lost"):
        out.append(_achado("dispositivo_perdido", True, "a placa parou de responder durante a execução",
                           "ver o motivo no resumo; os números até ali não são de uma execução sadia"))
    est = s.estavel()
    janelas = (est.get("window_medians") or {}).get("samples", 0)
    if r.get("first_world_frame_s") is None:
        out.append(_achado("sem_mundo", True, "a execução não chegou ao mundo",
                           "ver avisos e erros do carregamento; não há quadro para medir"))
    elif "stable_scene" not in r:
        # Resumo de antes da cena estável: o motor usa as medianas do mundo, sem este critério.
        pass
    elif est.get("from_s") is None:
        out.append(_achado("sem_cena_estavel", True,
                           f"a cena estável ({est.get('draws_at_least', '?')}+ desenhos por quadro) não foi alcançada",
                           "rodar por mais tempo, ou ajustar SPARK_STABLE_DRAWS/SPARK_STABLE_AFTER_S à cena"))
    elif janelas < JANELAS_MIN:
        out.append(_achado("poucas_amostras", True,
                           f"só {janelas} janelas de 1 s da cena estável (mínimo {JANELAS_MIN})",
                           "rodar por mais tempo antes de julgar"))
    disp = s.criacao_do_dispositivo_s()
    if disp is not None and disp > DISPOSITIVO_LENTO_S:
        out.append(_achado("dispositivo_lento", True,
                           f"o dispositivo D3D12 levou {disp:.0f} s para ser criado (normal: menos de 10 s)",
                           "limpar o cache de shaders do driver e repetir; o driver está num estado lento"))
    cache = (r.get("measurement") or {}).get("driver_cache_mb")
    if cache is not None and cache > CACHE_DO_DRIVER_MB:
        out.append(_achado("cache_do_driver", True,
                           f"o cache de shaders do driver tinha {cache / 1024:.1f} GB no início da execução",
                           "apagar o cache do driver (ele o refaz) e repetir"))
    mem = r.get("memory") or {}
    nos_longos = mem.get("page_faults_per_s_in_long_frames") or 0
    media = mem.get("page_faults_per_s_mean")
    longos = r.get("long_world_frames_over_1s", 0)
    # Com as causas por quadro (resumo novo), só é tempestade se algum quadro longo ficou sem fase que o explique.
    causas = r.get("long_frame_causes")
    sem_fase = not causas or any(c["cause"] in ("page_faults", "unexplained") for c in causas)
    if (longos and sem_fase and nos_longos > FALTAS_POR_S
            and (media is None or nos_longos > FALTAS_VEZES_A_MEDIA * media)):
        out.append(_achado("tempestade_de_faltas", s.parte_em_quadros_longos_pct() >= FALTAS_PARTE_GRAVE_PCT,
                           f"{longos} quadro(s) de mais de 1 s com {nos_longos:.0f} falhas de página por segundo "
                           f"(média da execução: {media or 0:.0f})",
                           "fechar o que usa memória, reduzir os orçamentos (VICTORY_*_BUDGET_MB) e repetir"))
    livre = mem.get("ram_free_min_mb")
    if livre is not None and livre < RAM_ALERTA_MB:
        out.append(_achado("ram_baixa", livre < RAM_GRAVE_MB, f"RAM livre mínima de {livre} MB",
                           "liberar memória e repetir: com pouca RAM o sistema pagina e os quadros longos são dele"))
    linhas = s.fatos["linhas_estaveis_ms"]
    fazendo = est.get("windows_making_pipelines")
    parte = fazendo / janelas if fazendo is not None and janelas else \
        s.fatos["linhas_fazendo_pipelines"] / len(linhas) if linhas else 0.0
    if parte > CACHES_FRIOS_PARTE:
        out.append(_achado("caches_frios", True,
                           f"{parte * 100:.0f} % das amostras da cena estável tinham pipelines ainda sendo feitas",
                           "rodar `python scripts/autopilot.py aquecer` e uma execução de aquecimento antes de medir"))
    if len(linhas) >= 8:
        baixo, mediana, alto = intervalo_da_mediana(linhas)
        meia = (alto - baixo) / 2 / mediana * 100 if mediana else 0.0
        if meia > IC_LARGO_PCT:
            out.append(_achado("cena_instavel", False,
                               f"o IC 95 % da mediana do quadro tem ±{meia:.0f} % (a cena variou muito)",
                               "preferir o A/B na execução (SPARK_AB) a comparar execuções"))
    if r.get("ab"):
        out.append(_achado("ab_na_execucao", False,
                           "a execução alternou chaves (SPARK_AB): as fases e os problemas misturam os braços",
                           "para o efeito das chaves, `motor.py ab`; para diagnosticar, uma execução sem A/B"))
    if r.get("events_dropped"):
        out.append(_achado("eventos_perdidos", False,
                           f"{r['events_dropped']} eventos perdidos (fila do log cheia)",
                           "o eventos.jsonl está incompleto; o resumo não é afetado"))
    out.sort(key=lambda a: not a["grave"])
    return out


def graves(achados: list[dict]) -> list[dict]:
    return [a for a in achados if a["grave"]]
