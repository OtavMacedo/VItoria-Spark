"""Testes da coordenação pelas issues, sem rede: python -m unittest discover -s scripts/tests"""

import sys
import unittest
from datetime import datetime, timezone
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
from motor import coordenacao as c  # noqa: E402

AGORA = datetime(2026, 11, 27, 12, tzinfo=timezone.utc).timestamp()


def issue(numero: int, alvo: str | None, sessao: str = "a", horas: float = 1, andamento: bool = True,
          estado: str = "OPEN") -> dict:
    quando = datetime.fromtimestamp(AGORA - horas * 3600, timezone.utc).isoformat().replace("+00:00", "Z")
    corpo = (c.marca(alvo, sessao) + "\n**Alvo:** x") if alvo else "sem marca"
    rotulos = [{"name": "autopilot"}] + ([{"name": c.ROTULO}] if andamento else [])
    return {"number": numero, "title": "t", "body": corpo, "labels": rotulos, "updatedAt": quando, "state": estado}


class Coordenacao(unittest.TestCase):
    def reivs(self, *issues):
        return c.reivindicacoes(list(issues), AGORA)

    def test_alvo_sem_issue_esta_livre(self):
        self.assertEqual(c.decidir("fase:copy_back", "a", self.reivs(issue(1, None))), {"acao": "livre", "issue": None})

    def test_outra_sessao_com_status_recente_ocupa_o_alvo(self):
        d = c.decidir("fase:copy_back", "b", self.reivs(issue(4, "fase:copy_back", "a", horas=3)))
        self.assertEqual(d, {"acao": "ocupado", "issue": 4})

    def test_a_propria_sessao_segue_na_sua_issue(self):
        d = c.decidir("fase:copy_back", "a", self.reivs(issue(4, "fase:copy_back", "a")))
        self.assertEqual(d["acao"], "minha")

    def test_sem_status_alem_da_validade_o_alvo_pode_ser_retomado(self):
        d = c.decidir("fase:copy_back", "b", self.reivs(issue(4, "fase:copy_back", "a", horas=30)))
        self.assertEqual(d, {"acao": "retomar", "issue": 4})

    def test_liberada_sem_rotulo_pode_ser_retomada(self):
        d = c.decidir("fase:copy_back", "b", self.reivs(issue(4, "fase:copy_back", "a", andamento=False)))
        self.assertEqual(d["acao"], "retomar")

    def test_duas_reivindicacoes_vale_a_mais_antiga(self):
        r = self.reivs(issue(9, "pipelines_recusadas", "b"), issue(7, "pipelines_recusadas", "a"))
        self.assertEqual(c.dono("pipelines_recusadas", r)["numero"], 7)

    def test_issue_fechada_nao_conta(self):
        self.assertEqual(self.reivs(issue(3, "fase:gpu", estado="CLOSED")), [])

    def test_anotar_marca_so_os_alvos_de_outras_sessoes(self):
        r = self.reivs(issue(5, "fase:gpu", "a"), issue(6, "memoria", "b"))
        ps = c.anotar([{"id": "fase:gpu"}, {"id": "memoria"}, {"id": "descartes"}], r, "b")
        self.assertEqual([p.get("em_andamento") for p in ps], [5, None, None])

    def test_o_titulo_diz_etapa_alvo_e_problema(self):
        self.assertEqual(c.titulo("em andamento", "fase:copy_back", "cópia de volta 70 ms/quadro (29 %)"),
                         "[em andamento] fase:copy_back: cópia de volta 70 ms/quadro (29 %)")

    def test_titulo_longo_e_cortado(self):
        t = c.titulo("em andamento", "pipeline:02384a60fda6f2d0", "x" * 200)
        self.assertEqual(len(t), c.TITULO_MAX)
        self.assertTrue(t.endswith("…"))

    def test_trocar_a_etapa_mantem_o_resto(self):
        self.assertEqual(c.trocar_etapa("[em andamento] fase:gpu: GPU 125 ms/quadro (52 %)", "A/B"),
                         "[A/B] fase:gpu: GPU 125 ms/quadro (52 %)")

    def test_resumo_de_uma_fase_traz_o_custo(self):
        p = {"titulo": "cópia de volta", "custo_ms": 70.4, "parte_pct": 29.3, "evidencia": ["x"]}
        self.assertEqual(c.resumo_do_problema(p), "cópia de volta 70 ms/quadro (29 %)")

    def test_resumo_sem_custo_usa_a_primeira_evidencia(self):
        p = {"titulo": "pipelines recusadas", "evidencia": ["2 de computação"]}
        self.assertEqual(c.resumo_do_problema(p), "pipelines recusadas: 2 de computação")

    def test_resumo_vago_longo_ou_com_caminho_e_recusado(self):
        self.assertIsNotNone(c.validar_resumo(""))
        self.assertIsNotNone(c.validar_resumo("y" * (c.RESUMO_MAX + 1)))
        self.assertIsNotNone(c.validar_resumo(r"falha em C:\pasta\arquivo"))
        self.assertIsNone(c.validar_resumo("GPU 125 ms/quadro (52 %)"))

    def test_quem_escreve_entrega_direto_e_os_demais_por_pr(self):
        for permissao in ("ADMIN", "MAINTAIN", "WRITE", "write"):
            self.assertEqual(c.modo_de_entrega(permissao), "direta")
        for permissao in ("TRIAGE", "READ", "NONE", ""):
            self.assertEqual(c.modo_de_entrega(permissao), "pr")

    def test_a_marca_se_le_de_volta(self):
        m = c.MARCA.search("texto\n" + c.marca("pipeline:02384a60fda6f2d0", "pc-12") + "\nmais")
        self.assertEqual((m["alvo"], m["sessao"]), ("pipeline:02384a60fda6f2d0", "pc-12"))


if __name__ == "__main__":
    unittest.main()
