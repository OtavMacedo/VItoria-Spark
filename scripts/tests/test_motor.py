"""Testes do motor de diagnóstico, só com dados sintéticos: python -m unittest discover -s scripts/tests"""

import contextlib
import io
import json
import math
import re
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
from motor import conhecimento, estat, medicao, offline, plano, problemas, relatorio, veredito  # noqa: E402
from motor.cli import main as _main  # noqa: E402
from motor.sessao import RAIZ, Sessao, dentro_do_repo  # noqa: E402


def main(args: list[str]) -> int:
    """O comando, com a saída calada."""
    with contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
        return _main(args)


def fase(nome: str, media: float, quadro: float, p95: float | None = None) -> dict:
    return {"name": nome, "mean_ms": media, "p50_ms": media, "p95_ms": p95 or media, "p99_ms": p95 or media,
            "max_ms": p95 or media, "share_pct": media / quadro * 100}


def resumo(**mais) -> dict:
    """Uma execução sadia: cena estável de 100 ms por quadro, 40 na placa, 30 de cópias, 20 remapeando."""
    partes = [fase("preparing", 45, 100), fase("copies", 30, 100, p95=120), fase("write_watch", 5, 100),
              fase("textures", 4, 100), fase("gpu", 40, 100), fase("copy_back", 1, 100), fase("present", 60, 100),
              fase("remap", 20, 100), fase("pipeline_wait", 0, 100), fase("card_idle", 2, 100),
              fase("disk", 0, 100), fase("pace", 0, 100)]
    r = {
        "session": {"env": {"VICTORY_COPY_PLAIN": "1"}},
        "duration_s": 400, "first_world_frame_s": 100, "end": "teste",
        "frames_world": {"frames": 3000, "p50_ms": 100, "p95_ms": 150, "p99_ms": 300, "max_ms": 900},
        "long_world_frames_over_1s": 0,
        "frame_rate_medians": {"world": True, "samples": 50, "ms": 100, "draws": 5000},
        "stable_scene": {"draws_at_least": 4500, "from_s": 160,
                         "frames": {"frames": 2400, "p50_ms": 100, "p95_ms": 150, "p99_ms": 300},
                         "window_medians": {"samples": 240, "ms": 100, "draws": 5000},
                         "windows_making_pipelines": 0},
        "phases": {"scope": "stable", "frames": 2400, "frame_mean_ms": 100, "parts": partes,
                   "copies_mb_per_frame": 30, "copies_mb_per_s": 1000, "copy_back_mb_per_frame": 1,
                   "copy_back_mb_per_s": 900, "watch_calls_per_frame": 300, "watch_pages_per_frame": 9000,
                   "watch_us_per_call": 16},
        "long_frame_causes": [], "dropped_by_cause": [], "untranslated_shaders": [],
        "measurement": {"device_create_ms": 6000, "driver_cache_mb": 300, "driver_cache_files": 40},
        "adapter": {"name": "Placa de teste", "driver": "1.2.3.4", "wave_max": "32"},
        "refused_pipelines": [], "refused_pipelines_total": 0, "untranslated_instructions": [],
        "device_lost": [], "crash": None,
        "memory": {"ram_free_min_mb": 8000, "page_faults_per_s_mean": 2000, "page_faults_per_s_in_long_frames": 0,
                   "vram_used_max_mb": 6000, "vram_budget_min_mb": 11000, "shared_used_max_mb": 200},
        "warnings_top": [], "events_dropped": 0, "ab": None, "ab_experiments": [],
    }
    r.update(mais)
    return r


def celula_do_bloco(bloco: int, fatores: int) -> int:
    """A célula do bloco, como `victory_core::ab::cell_of`: o bit f é a paridade dos bits do bloco que o fator f
    olha (todos; todos menos o 0; daí em diante, todos menos o 0 e o f)."""
    def bits(f: int) -> int:
        return ~0 if f == 0 else ~1 if f == 1 else ~(1 | 1 << f)
    return sum((bin(bloco & bits(f) & 0xFFFFFFFF).count("1") & 1) << f for f in range(fatores))


def blocos(efeitos: list[float], n: int = 48, ruido: float = 0.01, base: float = 100.0) -> dict:
    """Um A/B (ou fatorial) sintético: `efeitos[f]` é o efeito log do fator f; deriva lenta e ruído fixo."""
    k = len(efeitos)
    semente, lista = 12345, []
    for i in range(n):
        celula = celula_do_bloco(i, k)
        semente = (semente * 1103515245 + 12345) % 2**31
        jitter = (semente / 2**31 - 0.5) * 2 * ruido
        y = math.log(base) + 0.002 * i + jitter + sum(e for f, e in enumerate(efeitos) if celula >> f & 1)
        lista.append({"i": i, "arm": celula & 1, "cell": celula, "n": 60, "p50_ms": math.exp(y),
                      "p95_ms": math.exp(y) * 1.3, "over_1s": 0})
    nomes = [f"VICTORY_F{f}=0/1" for f in range(k)]
    return {"factors": nomes, "block_stats": lista, "period_s": 20,
            "a": {"sets": "VICTORY_F0=0", "frames": 1000, "long_frames_over_1s": 0, "p95_ms": 130},
            "b": {"sets": "VICTORY_F0=1", "frames": 1000, "long_frames_over_1s": 0, "p95_ms": 130}}


class ComSessao(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)

    def sessao(self, r: dict, eventos: list[dict] | None = None, nome: str = "teste-sessao") -> Sessao:
        pasta = Path(self.tmp.name) / nome / "20260101T000000Z-1"
        pasta.mkdir(parents=True, exist_ok=True)
        (pasta / "resumo.json").write_text(json.dumps(r), encoding="utf-8")
        if eventos is not None:
            (pasta / "eventos.jsonl").write_text("\n".join(json.dumps(e) for e in eventos), encoding="utf-8")
        return Sessao(pasta.parent)


class Estatistica(unittest.TestCase):
    def test_ab_acha_um_ganho_de_5_por_cento(self):
        v = estat.analisar(blocos([-0.05]), teto=True)
        self.assertEqual(v["estado"], "GANHO")
        self.assertAlmostEqual(v["efeito_pct"], -4.88, delta=0.6)
        self.assertLess(v["ic95_pct"][1], 0)

    def test_aa_nao_inventa_efeito(self):
        v = estat.analisar(blocos([0.0]), teto=True)
        self.assertEqual(v["estado"], "SEM_EFEITO")

    def test_poucos_blocos_continuam_e_no_teto_ficam_inconclusivos(self):
        ab = blocos([-0.05], n=6)
        self.assertEqual(estat.analisar(ab)["estado"], "CONTINUA")
        self.assertEqual(estat.analisar(ab, teto=True)["estado"], "INCONCLUSIVO")

    def test_fatorial_atribui_o_efeito_a_cada_fator(self):
        v = estat.analisar(blocos([-0.06, 0.0, 0.05]), teto=True)
        estados = [f["estado"] for f in v["fatores"]]
        self.assertEqual(estados, ["GANHO", "SEM_EFEITO", "REGRESSAO"])
        self.assertEqual(v["estado"], "DECIDIDO")
        self.assertEqual(v["interacoes"], [])
        for f, esperado in zip(v["fatores"], (-5.8, 0.0, 5.1)):
            self.assertAlmostEqual(f["efeito_pct"], esperado, delta=0.7)
            self.assertEqual(sum(f["blocos"]), 48)

    def test_fatorial_acusa_interacao(self):
        ab = blocos([-0.02, -0.02])
        for b in ab["block_stats"]:
            if b["cell"] == 3:
                b["p50_ms"] *= 1.15
        v = estat.analisar_fatorial(ab, teto=True)
        self.assertEqual(len(v["interacoes"]), 1)
        self.assertTrue(all(f.get("interage_com") for f in v["fatores"]))
        self.assertNotIn("GANHO", [f["estado"] for f in v["fatores"]])

    def test_fatorial_para_quando_todos_decidem(self):
        self.assertEqual(estat.analisar(blocos([-0.06, 0.05], n=8))["estado"], "CONTINUA")
        self.assertEqual(estat.analisar(blocos([-0.06, 0.05], n=32))["estado"], "DECIDIDO")

    def test_fisher_e_intervalo(self):
        self.assertEqual(estat.fisher_mais(0, 100, 0, 100), 1.0)
        self.assertLess(estat.fisher_mais(0, 500, 9, 500), 0.01)
        baixo, mediana, alto = estat.intervalo_da_mediana([float(x) for x in range(1, 102)])
        self.assertEqual(mediana, 51.0)
        self.assertTrue(baixo < mediana < alto)

    def test_entre_execucoes(self):
        self.assertEqual(estat.entre_execucoes([100, 101], [90])["estado"], "PROBLEMA_DE_MEDICAO")
        self.assertEqual(estat.entre_execucoes([100, 101, 99], [90, 91, 89])["estado"], "GANHO")


class Medicao(ComSessao):
    def ids(self, r: dict, eventos=None) -> dict:
        return {a["id"]: a["grave"] for a in medicao.verificar(self.sessao(r, eventos))}

    def test_execucao_sadia_nao_tem_achado(self):
        self.assertEqual(self.ids(resumo()), {})

    def test_dispositivo_lento_e_cache_do_driver_sao_graves(self):
        r = resumo(measurement={"device_create_ms": 45000, "driver_cache_mb": 31000})
        self.assertEqual(self.ids(r), {"dispositivo_lento": True, "cache_do_driver": True})

    def test_criacao_do_dispositivo_sai_dos_eventos_em_execucoes_antigas(self):
        r = resumo(measurement=None)
        eventos = [{"t": 0.5, "message": "D3D12.X device", "fields": {}},
                   {"t": 44.5, "message": "host adapter", "fields": {"spark.kind": "adapter"}}]
        self.assertEqual(self.ids(r, eventos), {"dispositivo_lento": True})

    def test_poucas_amostras_sem_cena_e_sem_mundo(self):
        r = resumo()
        r["stable_scene"]["window_medians"]["samples"] = 7
        self.assertEqual(self.ids(r), {"poucas_amostras": True})
        r["stable_scene"]["from_s"] = None
        self.assertEqual(self.ids(r), {"sem_cena_estavel": True})
        r["first_world_frame_s"] = None
        self.assertEqual(self.ids(r), {"sem_mundo": True})
        antigo = resumo()
        del antigo["stable_scene"]
        self.assertEqual(self.ids(antigo), {})

    def test_faltas_de_pagina_e_ram(self):
        r = resumo(long_world_frames_over_1s=30)
        r["memory"].update(page_faults_per_s_in_long_frames=400000, ram_free_min_mb=300)
        self.assertEqual(self.ids(r), {"tempestade_de_faltas": True, "ram_baixa": True})
        r = resumo(long_world_frames_over_1s=2)
        r["memory"].update(page_faults_per_s_in_long_frames=400000, ram_free_min_mb=900)
        self.assertEqual(self.ids(r), {"tempestade_de_faltas": False, "ram_baixa": False})
        # Quadros longos que uma fase explica não são tempestade, mesmo com muitas falhas de página.
        r["long_frame_causes"] = [{"cause": "remap", "frames": 2, "ms": 3000}]
        self.assertEqual(self.ids(r), {"ram_baixa": False})

    def test_caches_frios(self):
        r = resumo()
        r["stable_scene"]["windows_making_pipelines"] = 100
        self.assertEqual(self.ids(r), {"caches_frios": True})


class Diagnostico(ComSessao):
    def test_ordem_por_custo_com_evidencia_chaves_e_codigo(self):
        d = problemas.diagnosticar(self.sessao(resumo()))
        self.assertEqual(d["estado"], "OK")
        ids = [p["id"] for p in d["problemas"]]
        self.assertEqual(ids[:3], ["fase:gpu", "fase:copies", "fase:remap"])
        copias = d["problemas"][1]
        self.assertEqual(copias["custo_ms"], 30)
        self.assertTrue(any("cauda" in e for e in copias["evidencia"]))
        self.assertTrue(any("taxa baixa" in e for e in copias["evidencia"]))
        alternaveis = [c["nome"] for c in copias["chaves"] if c["alternavel"]]
        self.assertIn("VICTORY_COPY_PLAIN", alternaveis)
        self.assertTrue(all((RAIZ / c).exists() for c in copias["codigo"]))
        self.assertEqual(d["quadro"]["limite"], "CPU (preparo)")

    def test_problema_de_medicao_e_dito_no_lugar_de_um_veredito(self):
        r = resumo(measurement={"device_create_ms": 45000})
        d = problemas.diagnosticar(self.sessao(r))
        self.assertEqual(d["estado"], "PROBLEMA_DE_MEDICAO")
        texto = relatorio.diagnostico(d)
        self.assertIn("NÃO valem", texto)
        self.assertIn("45 s", texto)

    def test_traducao_descartes_memoria_e_imagem_na_mesma_lista(self):
        r = resumo(
            untranslated_instructions=[{"key": "v_teste_b32", "count": 4, "distinct_shaders": 4, "detail": "x"}],
            untranslated_shaders=[{"hash": "00112233aabbccdd", "stage": "pixel", "instruction": "v_teste_b32"}],
            refused_pipelines=[{"key": "[compute 8899aabbccddeeff]", "count": 1,
                                "detail": "host pipeline refused (the program needs waves of 64 to 64 lanes)"}],
            dropped_by_cause=[{"cause": "builder_off", "per_frame_median": 380, "on_purpose": True},
                              {"cause": "untranslated", "per_frame_median": 40, "on_purpose": False}],
            warnings_top=[{"level": "WARN", "target": "victory_gpu::queue", "count": 11, "first_s": 3.0,
                           "message": "a black frame after a frame that was not"}],
            long_world_frames_over_1s=12,
            long_frame_causes=[{"cause": "remap", "frames": 9, "ms": 14000}, {"cause": "gpu", "frames": 3, "ms": 4000}],
        )
        r["memory"]["shared_used_max_mb"] = 6000
        s = self.sessao(r)
        s.offline.mkdir()
        (s.offline / "00112233aabbccdd.pixel.cdxil").write_bytes(b"x")
        d = problemas.diagnosticar(s)
        por = {p["id"]: p for p in d["problemas"]}
        for esperado in ("shaders_nao_traduzidos", "pipelines_recusadas", "descartes", "descartes_de_proposito",
                         "memoria", "imagem", "quadros_longos"):
            self.assertIn(esperado, por)
        self.assertEqual(por["shaders_nao_traduzidos"]["offline"], ["00112233aabbccdd"])
        self.assertIn("ondas mais largas", por["pipelines_recusadas"]["sugestao"])
        self.assertIn("remap", por["quadros_longos"]["sugestao"])
        self.assertEqual(por["descartes"]["causas"][0]["cause"], "untranslated")
        self.assertEqual(por["descartes_de_proposito"]["pontos"], 1.0)
        pontos = [p["pontos"] for p in d["problemas"]]
        self.assertEqual(pontos, sorted(pontos, reverse=True))

    def test_execucao_antiga_usa_medianas_e_o_log_de_quadros(self):
        r = resumo(phases=None, dropped_by_cause=None)
        r["stable_scene"]["window_medians"].update(preparing_ms=45, copies_ms=30, textures_ms=4, gpu_ms=40,
                                                   copy_back_ms=1)
        usual = ("usual frame (the mean of 9 not long): 200 ms | render thread: ran 1 ms; in the present call 90.0 "
                 "(lines) | remapping: 100 calls 50.0 ms (1.0 waiting) | pipelines: 0 made, 0 loaded, 0.0 ms waited "
                 "| card: idle 3.0 ms | disk: 2 reads 0.1 MB 9.0 worker ms")
        eventos = [{"t": 200, "message": usual, "fields": {}},
                   {"t": 201, "message": "dropped 300.0/frame without a host pipeline: a stage of the ray-tracing "
                                         "structure builder, which is off", "fields": {}},
                   {"t": 201, "message": "dropped 2.0/frame without a host pipeline: refused [compute 0011223344556677]",
                    "fields": {}}]
        s = self.sessao(r, eventos)
        f = s.fases()
        self.assertEqual(f["fonte"], "medianas")
        # 50 ms de um quadro usual de 200 ms: 25 % do quadro da cena estável (100 ms).
        self.assertAlmostEqual(f["partes"]["remap"]["mean_ms"], 25.0)
        self.assertEqual([(c["cause"], c["on_purpose"]) for c in s.descartes()],
                         [("builder_off", True), ("refused", False)])
        d = problemas.diagnosticar(s)
        self.assertIn("fase:remap", [p["id"] for p in d["problemas"]])

    def test_sinal_de_imagem_fora_dos_dez_avisos_mais_frequentes_vem_dos_eventos(self):
        eventos = [{"t": 30.0, "level": "WARN", "target": "victory_gpu::descriptor::host",
                    "message": "buffer view the host would refuse left null", "fields": {}}]
        d = problemas.diagnosticar(self.sessao(resumo(), eventos))
        imagem = next(p for p in d["problemas"] if p["id"] == "imagem")
        self.assertEqual([(x["vezes"], x["target"]) for x in imagem["sinais"]],
                         [(1, "victory_gpu::descriptor::host")])

    def test_falha_vem_primeiro(self):
        r = resumo(crash={"what": "panic", "code": None, "location": "victory.exe+0x10", "at_s": 12, "stack": []})
        d = problemas.diagnosticar(self.sessao(r))
        self.assertEqual((d["estado"], d["problemas"][0]["id"]), ("FALHA", "falha"))


class Plano(ComSessao):
    def test_todas_as_chaves_alternaveis_num_plano_fatorial(self):
        d = problemas.diagnosticar(self.sessao(resumo()))
        p = plano.montar(d)
        chaves = [c["chave"] for c in p["alternaveis"]]
        self.assertEqual(len(chaves), len(set(chaves)))
        self.assertTrue(set(chaves) <= set(conhecimento.alternaveis()))
        self.assertTrue(all(e["fatores"] <= conhecimento.FATORES_MAX for e in p["experimentos"]))
        texto = plano.toml(p)
        self.assertIn('ab = "', texto)
        self.assertEqual(texto.count("[[experimento]]"), len(p["experimentos"]))
        self.assertEqual(sum(e["ab"].count("*") + 1 for e in p["experimentos"]), len(chaves))
        self.assertIn("SPARK_PLAN=", plano.md(p, "plano.toml"))

    def test_chave_inerte_fica_fora(self):
        r = resumo()
        r["session"]["env"]["VICTORY_QUEUE2"] = "3"
        p = plano.montar(problemas.diagnosticar(self.sessao(r)))
        self.assertNotIn("VICTORY_MERGE_MASK", [c["chave"] for c in p["alternaveis"]])

    def test_sem_contenier_guardado_pede_spark_offline(self):
        r = resumo(refused_pipelines=[{"key": "[compute 8899aabbccddeeff]", "count": 1, "detail": "refused"}])
        p = plano.montar(problemas.diagnosticar(self.sessao(r)))
        self.assertEqual(p["guardar_offline"], ["pipelines_recusadas"])
        self.assertIn("SPARK_OFFLINE=1", plano.md(p))


class Veredito(ComSessao):
    def test_ab_ganho_e_regressao_por_quadros_longos(self):
        v = veredito.ab(self.sessao(resumo(ab=blocos([-0.05]))))
        self.assertEqual(v["estado"], "GANHO")
        ab = blocos([-0.05])
        ab["b"]["long_frames_over_1s"] = 12
        v = veredito.ab(self.sessao(resumo(ab=ab)))
        self.assertEqual((v["estado"], v["motivo"]), ("REGRESSAO", "falhou: longos"))
        self.assertLessEqual(len(veredito.md(v).splitlines()), 20)

    def test_sem_ab_e_sem_cena_estavel(self):
        self.assertEqual(veredito.ab(self.sessao(resumo()))["estado"], "PROBLEMA_DE_MEDICAO")
        r = resumo(ab=blocos([-0.05]))
        r["stable_scene"]["from_s"] = None
        self.assertEqual(veredito.ab(self.sessao(r))["motivo"], "sem cena estável")

    def test_fatorial_da_um_veredito_por_fator(self):
        v = veredito.ab(self.sessao(resumo(ab=blocos([-0.06, 0.0, 0.05]))))
        self.assertEqual([f["estado"] for f in v["fatores"]], ["GANHO", "SEM_EFEITO", "REGRESSAO"])
        self.assertEqual(v["estado"], "REGRESSAO")
        texto = veredito.md(v)
        self.assertIn("`VICTORY_F0=0/1`: **GANHO**", texto)
        self.assertIn("interações: nenhuma", texto)
        linhas = veredito.registros(v, None, "abc", "pasta", "2026-01-01 00:00")
        self.assertEqual([x["chave"] for x in linhas], ["VICTORY_F0=0/1", "VICTORY_F1=0/1", "VICTORY_F2=0/1"])

    def test_ganho_pequeno_e_sem_efeito(self):
        v = veredito.ab(self.sessao(resumo(ab=blocos([-0.02], ruido=0.002))))
        self.assertEqual((v["estado"], v["motivo"]), ("SEM_EFEITO", "ganho abaixo de 3.0 %"))

    def test_entre_execucoes_bloqueia_com_problema_de_medicao(self):
        def lado(ms: list[float], nome: str, **mais) -> list[Sessao]:
            out = []
            for i, valor in enumerate(ms):
                r = resumo(**mais)
                r["stable_scene"]["frames"]["p50_ms"] = valor
                out.append(self.sessao(r, nome=f"{nome}{i}-sessao"))
            return out

        a, b = lado([100, 101, 99], "a"), lado([90, 91, 89], "b")
        v = veredito.entre_execucoes(a, b)
        self.assertEqual(v["estado"], "GANHO")
        self.assertEqual(v["fases"][0]["dif_ms"], 0)
        lento = lado([90, 91, 89], "c", measurement={"device_create_ms": 50000})
        v = veredito.entre_execucoes(a, lento)
        self.assertEqual(v["estado"], "PROBLEMA_DE_MEDICAO")
        self.assertIn("dispositivo", v["motivo"])
        self.assertEqual(veredito.entre_execucoes(a, lento, sem_medicao=True)["estado"], "GANHO")


class Offline(ComSessao):
    MANIFESTO = [
        {"kind": "untranslated", "hash": "00112233aabbccdd", "stage": "pixel", "bytes": 10,
         "file": "00112233aabbccdd.pixel.cdxil", "error": "v_teste is not supported", "pipeline": ""},
        {"kind": "refused", "hash": "8899aabbccddeeff", "stage": "compute", "bytes": 10,
         "file": "8899aabbccddeeff.compute.cdxil", "error": "E_INVALIDARG", "pipeline": "compute 8899aabbccddeeff"},
        {"kind": "refused", "hash": "8899aabbccddeeff", "stage": "compute", "bytes": 10,
         "file": "8899aabbccddeeff.compute.cdxil", "error": "E_INVALIDARG", "pipeline": "compute 8899aabbccddeeff"},
        {"kind": "refused", "hash": "1111111111111111", "stage": "vertex", "bytes": 10, "file": None,
         "error": "e", "pipeline": ""},
    ]
    SAIDA = ("containers   2 (0 duplicates skipped)\n"
             "<X>/offline/00112233aabbccdd.pixel.cdxil@0x0 #00112233aabbccdd ~0a0b0c0d Ps  pipeline created\n"
             "<X>/offline/8899aabbccddeeff.compute.cdxil@0x0 #8899aabbccddeeff ~01020304 Cs  pipeline: refused\n"
             "        1  pipeline: refused  (e.g. <X>/offline/8899aabbccddeeff.compute.cdxil@0x0 Cs)\n")

    def test_cada_falha_guardada_e_julgada_contra_o_build_atual(self):
        s = self.sessao(resumo())
        s.offline.mkdir()
        (s.offline / "manifesto.jsonl").write_text("\n".join(json.dumps(x) for x in self.MANIFESTO), encoding="utf-8")
        manifesto = offline.ler_manifesto(s.offline)
        self.assertEqual(len(manifesto), 3)
        itens = offline.julgar(manifesto, offline.ler_saida(self.SAIDA))
        self.assertEqual([i["estado"] for i in itens], ["CORRIGIDO", "AINDA_FALHA", "SEM_ARQUIVO"])
        self.assertEqual(itens[1]["agora"], ["pipeline: refused"])
        texto = offline.md({"estado": "RESTAM_FALHAS", "itens": itens, "contagem": {"CORRIGIDO": 1}})
        self.assertIn("**AINDA_FALHA**", texto)

    def test_sem_manifesto_nao_ha_o_que_reproduzir(self):
        r = offline.reproduzir(self.sessao(resumo()))
        self.assertEqual(r["estado"], "SEM_DADOS")

    def test_ambiente_do_translate_vem_da_execucao(self):
        eventos = [{"t": 1, "message": "host view heaps hold 1000000 views; clamped", "fields": {"views": 1000000}}]
        env = offline.ambiente(self.sessao(resumo(), eventos), {"VICTORY_X": "1"})
        self.assertEqual((env["VICTORY_HOST_WAVE"], env["VICTORY_VIEW_LIMIT"], env["VICTORY_X"]),
                         ("32", "999999", "1"))


class Conhecimento(unittest.TestCase):
    def test_todo_arquivo_citado_existe(self):
        citados = [c for info in conhecimento.FASES.values() for c in info["codigo"]]
        citados += [c for lista in conhecimento.CODIGO.values() for c in lista]
        self.assertEqual([c for c in citados if not (RAIZ / c).exists()], [])

    def test_toda_chave_citada_existe_no_codigo(self):
        fontes = "".join(p.read_text(encoding="utf-8", errors="replace")
                         for pasta in ("crates", "apps", "perfis") for p in (RAIZ / pasta).rglob("*")
                         if p.suffix in (".rs", ".toml") and "target" not in p.parts)
        chaves = [c[0] for info in conhecimento.FASES.values() for c in info["chaves"]]
        chaves += [c[0] for c in conhecimento.CHAVES_DE_MEMORIA]
        chaves += [conhecimento.CHAVE_DO_CONSTRUTOR, conhecimento.CHAVE_DE_ONDA, *conhecimento.VALORES_AB,
                   *conhecimento.INERTE]
        self.assertEqual([c for c in chaves if f'"{c}"' not in fontes and f"{c} =" not in fontes], [])

    def test_alternaveis_saem_da_tabela_do_runtime(self):
        tabela = conhecimento.alternaveis()
        self.assertGreaterEqual(len(tabela), 7)
        self.assertEqual(tabela["VICTORY_COPY_PLAIN"], "copies")
        self.assertTrue(set(tabela.values()) <= set(conhecimento.FASES) | {"preparing"})
        self.assertEqual(conhecimento.espec_ab("VICTORY_TEXTURE_ROOM_PERCENT"), "VICTORY_TEXTURE_ROOM_PERCENT=80/94")

    def test_fases_e_causas_sao_as_do_runtime(self):
        fases = (RAIZ / "crates/victory-core/src/session/phases.rs").read_text(encoding="utf-8")
        nomes = re.search(r"PHASE_NAMES: \[&str; PHASES\] = \[(.*?)\];", fases, re.S).group(1)
        do_runtime = set(re.findall(r'"(\w+)"', nomes))
        self.assertTrue(set(conhecimento.FASES) - {"preparing_other"} <= do_runtime)
        maximo = re.search(r"MAX_FACTORS: usize = (\d+)",
                           (RAIZ / "crates/victory-core/src/ab.rs").read_text(encoding="utf-8")).group(1)
        self.assertEqual(int(maximo), conhecimento.FATORES_MAX)


class Comandos(ComSessao):
    def test_diagnosticar_grava_na_pasta_da_sessao_e_nunca_no_repositorio(self):
        s = self.sessao(resumo())
        self.assertEqual(main(["diagnosticar", str(s.pasta)]), 0)
        for nome in ("diagnostico.md", "diagnostico.json", "plano.toml", "plano.md"):
            self.assertTrue((s.pasta / nome).exists(), nome)
        d = json.loads((s.pasta / "diagnostico.json").read_text(encoding="utf-8"))
        self.assertEqual(d["problemas"][0]["id"], "fase:gpu")
        self.assertTrue(dentro_do_repo(RAIZ / "scripts"))
        self.assertFalse(dentro_do_repo(s.pasta))

    def test_ab_registra_uma_decisao(self):
        s = self.sessao(resumo(ab=blocos([-0.05])))
        decisoes = Path(self.tmp.name) / "decisoes.jsonl"
        self.assertEqual(main(["ab", str(s.pasta), "--registrar", str(decisoes), "--chave", "VICTORY_F0"]), 0)
        linha = json.loads(decisoes.read_text(encoding="utf-8"))
        self.assertEqual((linha["chave"], linha["estado"]), ("VICTORY_F0", "GANHO"))
        self.assertTrue((s.pasta / "veredito.md").exists())
        self.assertEqual(main(["consultar", str(decisoes), "VICTORY_F0"]), 0)

    def test_comando_desconhecido_e_sessao_ausente(self):
        self.assertEqual(main(["nada"]), 2)
        self.assertEqual(main(["diagnosticar", str(Path(self.tmp.name) / "nao-existe")]), 1)


if __name__ == "__main__":
    unittest.main()
