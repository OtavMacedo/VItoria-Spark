<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/banner-escuro.svg">
  <source media="(prefers-color-scheme: light)" srcset="assets/banner-claro.svg">
  <img src="assets/banner-claro.svg" alt="VItoria Spark: compatibilidade Xbox Series no Windows, em Rust e Direct3D 12">
</picture>

![Licença GPL-3.0](https://img.shields.io/badge/licen%C3%A7a-GPL--3.0-2ea44f) ![Windows x64](https://img.shields.io/badge/plataforma-Windows%20x64-0078d4) ![Rust](https://img.shields.io/badge/Rust-est%C3%A1vel-dea584) ![Projeto brasileiro](https://img.shields.io/badge/projeto-brasileiro-009c3b) [![AutoPilot Mode](https://img.shields.io/badge/%E2%9A%A1%20AutoPilot%20Mode-contribua%20com%20IA-facc15)](#-autopilot-mode)

**VItoria Spark** é uma camada de compatibilidade que roda software de Xbox Series no Windows x64. É um projeto
brasileiro, fork do [VIctory](https://github.com/Marvisak/VIctory), focado em duas coisas: **mais quadros por segundo**
e **texturas e imagem corretas**.

O código do console já é x86-64, então roda direto no processador do PC. O Spark fornece o que falta: carregador,
serviços do sistema, áudio, entrada e a tradução da GPU do console para Direct3D 12 (shaders GFX10 → DXIL).

> O repositório não contém e nunca vai conter nenhum arquivo de jogo. Você usa os seus próprios arquivos, obtidos
> legalmente, e tudo o que é derivado deles fica fora do repositório. Ver [política de arquivos](docs/asset-policy.md).

## ⚡ AutoPilot Mode

> [!TIP]
> **Contribua sem escrever uma linha.** Aponte um agente de IA (Claude Code ou similar) para este repositório e para
> a pasta do seu título. Ele trabalha sozinho, com a janela escondida e sem som, e nada do jogo sai da sua máquina.

| 1. Medir | 2. Diagnosticar | 3. Corrigir | 4. Provar | 5. Entregar |
|:---:|:---:|:---:|:---:|:---:|
| roda o título escondido e gera o `resumo.md` | acha o maior gargalo e abre ou comenta a issue | implementa com testes | antes e depois, 2+ execuções de cada lado | abre o PR com os números |

**Como ligar, em três passos:**

1. Configure `victory.toml` com o `game_dir` da **sua** cópia, fora do repositório.
2. Aceite os limites uma vez, você mesmo: `python scripts/autopilot.py confirmar`.
3. Diga ao agente:

```text
Leia o CLAUDE.md e entre em AutoPilot Mode. Título em <pasta do jogo>.
```

Limites legais embutidos: só a sua cópia, obtida legalmente; nada de baixar ou compartilhar arquivos de jogo; nada
que contorne proteção de cópia; no máximo 3 issues e 1 PR por sessão. O ciclo completo está em
[docs/autopilot.md](docs/autopilot.md).

## Onde estamos

Medido em 26/11/2026 numa RTX 3060 12 GB, com Ryzen 5 5600GT e 32 GB de RAM.

**Já funciona**
- O título inicia, roda menus e vídeos a 30 fps, carrega o perfil e o save e entra no mundo 3D.
- O mundo aparece iluminado e com texturas, com personagens, cenário e HUD.
- O desempenho no mundo fica em **3,2–3,6 fps de mediana, com picos de 6,4 fps**, numa cena pesada (~4.750
  desenhos por quadro).
- Sessões de 9 minutos rodam sem crash e sem perda de dispositivo. O encerramento pelo watchdog na inicialização
  está resolvido (`VICTORY_NO_WATCHDOG=1`, ligado no perfil comum).
- O ambiente de contribuição está pronto:
  - logs com resumo por sessão e a seção "Próximo alvo";
  - perfis de ajustes por fabricante (NVIDIA, AMD, Intel);
  - AutoPilot Mode;
  - regras verificadas por hook e pela CI.

**Ainda não**
- **Jogável de verdade:** a meta é 30 fps; hoje o quadro leva ~280 ms, dividido entre a placa (~130–160 ms) e o
  preparo das cópias de memória (~115–125 ms).
- **Ray tracing:** o construtor das estruturas fica desligado, porque ligado derruba o FPS sem mudar a imagem. Áreas
  iluminadas só por luz indireta ficam mais escuras que no console.
- **Programas wave64 em placas de 32 lanes (NVIDIA):** o driver recusa cerca de 400 programas de compute do pacote,
  dois deles usados nas primeiras áreas. A forma de 32 lanes cobre só alguns por enquanto.
- **Memória:** com 32 GB de RAM ainda há quadros de mais de 1 s quando a RAM livre acaba.
- **AMD e Intel:** ainda sem medição publicada.

## Destaques

- **Desempenho:** cópias de memória para a placa por máscara de bytes, uma consulta de páginas alteradas por reserva,
  desenhos mesh lançados por estágio de amplificação, comparações na placa em grupos de 64 threads, limpeza dos
  shaders traduzidos (código invariante fora de laços, barreiras redundantes removidas).
- **Imagem:** correções de amostragem (LOD, bias, samplers com e sem comparação), formatos de textura, BCPack,
  interpolação, máscaras de lanes wave64 em GPUs de 32 lanes, sombras em cascata.
- **Logs feitos para análise:** cada sessão gera um resumo curto (`resumo.md`) e eventos estruturados (`eventos.jsonl`),
  prontos para você ou um assistente de IA encontrar o próximo gargalo. Ver [docs/logs.md](docs/logs.md).
- **Estabilidade:** proteção opcional contra o encerramento por watchdog em carregamentos lentos
  (`VICTORY_NO_WATCHDOG=1`).

## Desempenho medido

### Por fabricante

| Placa | Mediana no mundo | Picos | Cena | Situação |
|---|---|---|---|---|
| **NVIDIA** RTX 3060 12 GB | 3,2–3,6 fps | 6,4 fps | pesada, ~4.750 desenhos/quadro | medido (2 sessões de 9 min) |
| **AMD** | — | — | — | aguardando medição: [contribua](#-autopilot-mode) |
| **Intel** | — | — | — | aguardando medição |

Cada fabricante tem o seu perfil de ajustes em [`perfis/`](perfis), aplicado automaticamente pela placa detectada
(regras em [CONTRIBUTING.md](CONTRIBUTING.md#7-nvidia-amd-e-intel-juntas)).

### Evidência para o PortWatch

Evidência pública para o [PortWatch VI](https://portwatch-vi.xyz/). São dados do próprio projeto, medidos em
2026-11-26.

| Campo | Evidência |
|---|---|
| Desempenho | **3,2–3,6 fps de mediana, com picos de até 6,4 fps**, numa cena pesada (~4.750 desenhos por quadro); 30 fps nos menus |
| Estabilidade | 2 sessões de 9 min sem crash e sem perda de dispositivo; com `VICTORY_NO_WATCHDOG=1`, o encerramento na inicialização não volta |
| Hardware | Windows 11 Pro · Ryzen 5 5600GT · 32 GB de RAM · NVIDIA RTX 3060 12 GB · 1080p |
| Gargalo atual | a placa (~145–160 ms por quadro) e o preparo das cópias (~115–125 ms); a RAM chega ao limite e causa quadros longos |
| Código | público, GPL-3.0, com 259 testes do tradutor e 234 da GPU passando |

Mediana das amostras de quadro no mundo, duas execuções de 9 minutos com caches aquecidos.

## Requisitos

- Windows 10/11 x64 e GPU com Direct3D 12, Shader Model 6.6 e mesh shaders (NVIDIA RTX 20 ou mais nova, AMD RX 6000
  ou mais nova).
- 32 GB de RAM recomendados.
- Para compilar: Rust estável (versão em `rust-toolchain.toml`) e Visual Studio Build Tools com o Windows SDK.

## Compilar e rodar

```powershell
cargo build --release -p victory
copy victory.example.toml victory.toml   # ajuste game_dir e cache_dir
.\target\release\victory.exe run <executável> --execute
```

`victory.exe config` mostra a configuração resolvida. Build de diagnóstico, com instrumentação extra:
`cargo build --profile diag -p victory --features diagnostics`.

## Contribuir

Leia [CONTRIBUTING.md](CONTRIBUTING.md) antes do primeiro commit: as regras são rígidas e verificadas por hook e
pela CI. Para usar assistentes de IA no projeto, as instruções ficam em [CLAUDE.md](CLAUDE.md).

## Créditos e licença

Fork do [VIctory](https://github.com/Marvisak/VIctory). Os autores do código estão em [AUTORES.md](AUTORES.md).
Licença [GPL-3.0](LICENSE).
