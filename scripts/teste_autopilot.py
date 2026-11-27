"""Testes do autopilot.py que não abrem o título: leitura do victory.toml e do manifesto do pacote.

Uso: python -m unittest discover -s scripts -p "teste_*.py"
"""

import tempfile
import unittest
from pathlib import Path

import autopilot

# Manifesto sintético na forma do MicrosoftGame.config: o atributo de nome de exibição vem depois de Name.
MANIFESTO = """<?xml version="1.0" encoding="utf-8"?>
<Game configVersion="1">
  <ExecutableList>
    <Executable Name="titulo.exe" Id="App" TargetDeviceFamily="Scarlett" OverrideDisplayName="Nome de Exibicao" IsDevOnly="false" />
  </ExecutableList>
</Game>
"""


class GameDirDoToml(unittest.TestCase):
    def test_em_paths_como_no_exemplo(self):
        texto = (Path(autopilot.RAIZ) / "victory.example.toml").read_text(encoding="utf-8")
        self.assertEqual(autopilot.game_dir_do_toml(texto), "D:\\victory-data\\game")

    def test_no_topo(self):
        self.assertEqual(autopilot.game_dir_do_toml("game_dir = 'X:\\jogo'\n"), "X:\\jogo")

    def test_ausente(self):
        self.assertIsNone(autopilot.game_dir_do_toml("[paths]\ncache_dir = 'X:\\cache'\n"))


class ExecutaveisDoManifesto(unittest.TestCase):
    def test_ignora_override_display_name(self):
        self.assertEqual(autopilot.executaveis_do_manifesto(MANIFESTO), ["titulo.exe"])

    def test_dois_executaveis(self):
        texto = '<Executable Name="a.exe" Id="A" /><Executable Id="B" Name="b.exe" />'
        self.assertEqual(autopilot.executaveis_do_manifesto(texto), ["a.exe", "b.exe"])


class AcharJogo(unittest.TestCase):
    def test_pasta_com_manifesto_em_maiusculas_e_bom(self):
        with tempfile.TemporaryDirectory() as d:
            pasta = Path(d)
            (pasta / "MicrosoftGame.Config").write_text("\ufeff" + MANIFESTO, encoding="utf-8")
            (pasta / "titulo.exe").write_bytes(b"")
            (pasta / "outro.exe").write_bytes(b"")
            self.assertEqual(autopilot.achar_jogo(str(pasta)), pasta / "titulo.exe")


if __name__ == "__main__":
    unittest.main()
