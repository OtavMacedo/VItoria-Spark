# DLSS Super Resolution (NVIDIA Streamline)

`VICTORY_DLSS=1` (desligado por padrão): o DLSS Super Resolution faz o upscaling FidelityFX do próprio título (o
`ffxDispatch` dele), com a mesma cor, profundidade, vetores de movimento, exposição e máscara reativa, na mesma saída
e no mesmo ponto da lista de comandos do título. Sem a variável, nada muda.

Nada da NVIDIA está neste repositório. A biblioteca é carregada em tempo de execução de uma pasta que você indica. Se
faltar alguma coisa (a pasta, uma assinatura, o recurso nesta placa, a entrega do device), o log diz o quê e o título
segue com o próprio upscaler, como sem a variável.

## Preparação

1. Baixe a release do SDK do Streamline no GitHub oficial da NVIDIA (`NVIDIA-RTX/Streamline`, página Releases; a
   origem testou a `v2.14.16`) e descompacte fora do repositório. A pasta `bin\x64` dela tem o que é carregado:
   `sl.interposer.dll` (carregada por nós, com a assinatura Authenticode conferida), `sl.common.dll`, `sl.dlss.dll`
   (plugins; o próprio Streamline confere as assinaturas) e `nvngx_dlss.dll` (o DLSS). Essas DLLs têm licença
   própria da NVIDIA e **nunca** entram no repositório.
2. `VICTORY_STREAMLINE_DIR=<a pasta bin\x64>`. Não há padrão: sem a variável, o DLSS fica desligado.
3. Confira o PC sem o título (alguns segundos):
   - `victory upscaler-check`: o Streamline carrega e diz se esta placa tem DLSS.
   - `victory upscaler-check --device [--output 1080p|1440p|LxA]`: cria o device, entrega ao Streamline, roda o
     DLSS três vezes em texturas de teste no modo de `VICTORY_DLSS_MODE` e lista os tamanhos de render do DLSS para
     a saída. Termina com `Streamline shut down: ok (0)`.

Windows 10 2004 ou mais novo e uma GeForce RTX.

## Variáveis

| Variável | Valores | Padrão | O que faz |
|---|---|---|---|
| `VICTORY_DLSS` | `1` | desligado | DLSS no lugar do upscaling FidelityFX do título |
| `VICTORY_STREAMLINE_DIR` | uma pasta | nenhuma | onde estão `sl.interposer.dll` e o resto |
| `VICTORY_DLSS_MODE` | `dlaa`, `quality`, `balanced`, `performance`, `ultraperformance` | `quality` | cena a 1, 2/3, 0,58, 1/2 ou 1/3 da saída; o modo do DLSS segue os tamanhos que o título passa |
| `VICTORY_DLSS_PRESET` | `default`, `j`, `k`, `l`, `m` (`e`, `f` antigos) | `default` | o modelo do DLSS, para todos os modos |
| `VICTORY_DLSS_JITTER` | `x,y` | `1,1` | multiplicadores do jitter (`-1,1` inverte x) |
| `VICTORY_DLSS_MV` | `x,y` | `1,1` | multiplicadores da escala dos vetores de movimento |
| `VICTORY_DLSS_REACTIVE` | `0` | ligado | `0`: a máscara reativa do título não vai ao DLSS |
| `VICTORY_STREAMLINE_LOG` | uma pasta | nenhuma | log do próprio Streamline nessa pasta, detalhado |

Com `VICTORY_OUTPUT_RESOLUTION` (saída 1080p ou 1440p no perfil Series X), a cena fica na escala do modo do DLSS e
`VICTORY_SCENE_SCALE` é ignorada (o log avisa).

## Como funciona

**Onde o título faz o upscaling.** O executável importa `ffxCreateContext`, `ffxDestroyContext`, `ffxConfigure`,
`ffxQuery` e `ffxDispatch` de `amd_fidelityfx_loader_xbox.dll`. Com o DLSS disponível, essas importações passam
pelas nossas (`victory_hle::modules::fidelityfx`), que chamam a DLL do próprio título para tudo; só o `ffxDispatch` de
upscale é tomado. O contexto do upscaler continua sendo do título, então o upscaling dele roda de novo em qualquer
quadro que o DLSS não pegue. O FSR 4 do título só aceita os tamanhos do Series S (1280x720 para 1920x1080): com o
DLSS, o contexto dele é criado nesses tamanhos e o DLSS recebe os que o título pediu.

**O caminho FidelityFX no Series X.** Sozinho, o título só usa esse caminho (renderer 7) num Series S; no Series X
usa o próprio passe temporal (renderer 6). Com o DLSS disponível, o build conhecido recebe patches
(`victory_game::upscaler`): a seleção de renderer passa a dar 7 sempre, e o código que dimensiona o caminho anota o
modo de qualidade FidelityFX de `VICTORY_DLSS_MODE` e usa a escala desse modo. A escala da cena do passe temporal vai
para o mesmo valor. Cada trecho é conferido contra o hash BLAKE3 dos bytes originais antes; outro build é recusado.

**Exclusão com o patch do upscaler próprio.** A seleção de renderer fica dentro da função que o patch "upscaler
choice" troca por `mov eax, 6; ret`. Por isso os dois nunca vão juntos: com o DLSS disponível, os patches "upscaler
choice" ficam de fora e entram os do caminho FidelityFX; sem o DLSS (variável ausente, Streamline ausente ou placa
sem DLSS), nada muda. A linha `upscaler` no início diz qual entrou.

**No dispatch.** Na thread do título as texturas são conferidas (cópias no host, em estados que um shader lê ou
escreve). Depois, no ponto da lista em que o título está, as entradas são preparadas como as de um compute shader
(views em alias, uploads de texturas escritas pelo título, gêmeas de profundidade), vão para o estado que o título
informou e são passadas ao `slEvaluateFeature` como tags locais. A saída fica marcada como escrita no host, e o
estado de desenho do título é refeito antes do próximo desenho dele. Constantes do dispatch: jitter, escala dos
vetores, near, far, campo de visão vertical, reset; das flags do contexto: HDR, profundidade invertida, exposição
automática, vetores na resolução da saída e vetores com jitter.

## Linhas do log

No início: `upscaling      DLSS on (quality, ...)`, `Streamline: DLSS Super Resolution is supported on this GPU`,
`Streamline loaded (manual hooking, no downloads)`, a linha `upscaler` e as linhas `title patch ... upscaler path:`.
No device: `Streamline: DLSS Super Resolution loaded streamline=... ngx=...`. No título: `FidelityFX: the title's
import goes through ours`, `FidelityFX: the title made its upscaler`, `FidelityFX: the title's first upscale
dispatch` e `DLSS: upscaling recorded in place of the title's FidelityFX` (tamanhos, modo, formatos; de novo quando
mudam). A cada segundo: `DLSS per frame: ... upscalings by DLSS, ... failed, ... left to the title's FidelityFX`.
`FidelityFX: the title's own upscaling runs: <motivo>` aparece uma vez por motivo.

## Limites

- Só o Super Resolution. A geração de quadros da origem ficou de fora.
- As matrizes de câmera são montadas (o título não as passa); o DLSS Super Resolution não as lê.
- "presentCommon() was not observed" (do Streamline, uma vez por execução): nossa swap chain não passa pelo
  Streamline, então o plugin comum não vê os presents e só libera o que o DLSS soltou ao fim do processo.
- O contexto FidelityFX do título continua alocado ao lado do do DLSS.

Origem: trustno1-coder/VIctory-NVIDIA (GPL-3.0), commits fc8b9cae, d11233aa e 4fcaebdb de readmet3xt-lab; os layouts
FidelityFX e o patch do caminho FidelityFX vêm de DexNom/VIctory (GPL-3.0-or-later). A ABI do Streamline foi escrita
a partir dos headers do SDK (licença no estilo MIT da NVIDIA).
