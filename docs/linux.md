# Linux

No Linux o Spark roda como programa do Windows: o `victory.exe` vai para o Wine (ou o Wine de um Proton), e o
Direct3D 12 vai para o Vulkan pelo [vkd3d-proton](https://github.com/HansKristian-Work/vkd3d-proton). O código já
conhece esse caminho (detecta o Wine e ajusta a memória do título a ele). Tudo fica num prefixo dedicado, separado
dos seus outros prefixos.

> **Situação:** relatado pela comunidade em NVIDIA com vkd3d-proton (Wine 11 / GE-Proton, vkd3d-proton 3.x, RTX
> 3060). **Ainda não verificado pelos mantenedores no Linux**, incluindo o `scripts/linux/spark.sh`, que só passou
> por verificação estática (`bash -n` e shellcheck). Não há medição de desempenho no Linux publicada.

## Requisitos

- Linux x86-64 com uma placa e um driver Vulkan com `VK_EXT_mesh_shader` (NVIDIA RTX 20 ou mais nova com o driver
  proprietário; AMD RX 6000 ou mais nova com RADV recente). 32 GB de RAM recomendados.
- Wine 10 ou mais novo (o relato da comunidade é com Wine 11), ou um Proton (por exemplo GE-Proton) apontado em `SPARK_PROTON`.
- vkd3d-proton 3.x ou mais novo e o `dxgi.dll` do [DXVK](https://github.com/doitsujin/dxvk), que o vkd3d-proton
  usa para a swapchain. Um Proton já traz os dois. O DXVK-NVAPI não é necessário.
- O `d3dcompiler_47.dll` da Microsoft no prefixo (`winetricks d3dcompiler_47`). O do Wine e o do Proton não compilam
  os shaders HLSL do Spark; sem o da Microsoft, a execução indireta é descartada, as comparações de cópias na placa
  ficam desligadas e os planos de profundidade não são copiados. O `preparar` confere e o `rodar` o ativa como nativo.
- Para compilar no Linux: rustup, [cargo-xwin](https://github.com/rust-cross/cargo-xwin), clang, lld e llvm. Também
  dá para usar um `victory.exe` compilado no Windows (`VICTORY_EXE=<caminho>`).

## Três comandos

```bash
scripts/linux/spark.sh preparar      # confere o que falta e cria o prefixo; nunca usa sudo
scripts/linux/spark.sh compilar      # release e diag para x86_64-pc-windows-msvc
scripts/linux/spark.sh rodar <pasta-ou-exe-do-título> [VAR=valor ...]
```

- **`preparar`** confere Wine, Vulkan com mesh shaders, vkd3d-proton e DXVK no prefixo, `vm.max_map_count` e as
  ferramentas de compilação. Para cada item faltando, mostra o comando de instalação no Debian/Ubuntu, Fedora ou
  Arch. O prefixo fica em `~/.local/share/vitoria-spark/prefix` (`SPARK_WINEPREFIX` troca). Para instalar o
  vkd3d-proton e o DXVK nele, extraia as versões oficiais e rode
  `SPARK_VKD3D_DIR=<pasta> SPARK_DXVK_DIR=<pasta> scripts/linux/spark.sh preparar`; com `SPARK_PROTON`, ele copia
  os do Proton.
- **`compilar`** roda `cargo xwin build` com o mesmo `rust-toolchain.toml`. Na primeira vez, o cargo-xwin baixa o
  CRT e o SDK do Windows, sob a licença da Microsoft. O resultado fica em `target/x86_64-pc-windows-msvc/`. O alvo
  `x86_64-pc-windows-gnu` não é suportado.
- **`rodar`** ativa `d3d12`, `d3d12core` e `dxgi` nativos (`WINEDLLOVERRIDES`), usa `WINEDEBUG=-all`, guarda o
  cache de pipelines do vkd3d-proton em `~/.cache/vitoria-spark/vkd3d` e os logs da sessão em
  `~/.local/state/vitoria-spark/execucoes/<data>` (`SPARK_LOG_DIR`). Converte os caminhos com `winepath -w` e
  chama `victory.exe run <título> --execute`. Os saves e o cache do Spark ficam dentro do prefixo
  (`%LOCALAPPDATA%\victory`), a menos que `victory.toml` diga outra coisa.
- **Perfil de placa:** por padrão o DXVK diz ao DXGI que uma placa NVIDIA é AMD. O `rodar` exporta
  `DXVK_CONFIG="dxgi.hideNvidiaGpu = False"` (somado ao seu `DXVK_CONFIG`, se houver) para o Spark aplicar o perfil
  do fabricante certo. Quem roda o `victory.exe` por conta própria deve exportar o mesmo.

`scripts/linux/spark.sh diagnostico` imprime kernel, distro, CPU, RAM, GPU, driver, Vulkan, Wine e vkd3d-proton,
com a pasta pessoal trocada por `<HOME>`, para colar numa issue.

## Limites conhecidos

- O caminho Wine + vkd3d-proton foi relatado pela comunidade em NVIDIA; AMD e Intel no Linux ainda não têm relato.
- Um build nativo para Linux não roda títulos (`run` exige o Windows): o caminho é sempre o `.exe` sob o Wine.
- Ainda não verificado pelos mantenedores: a detecção da versão do vkd3d-proton (pode aparecer "versão não
  identificada"), a cópia das DLLs a partir de um Proton e o modo sem áudio do AutoPilot.

## Problemas comuns

- **Perda de dispositivo (device lost) ou a janela fecha sozinha:** rode `diagnostico` e confira o driver. Teste
  sem overlays (MangoHud, gravação). Abra uma issue com a saída do `diagnostico` e o código de saída, sem log.
- **"VK_EXT_mesh_shader" faltando:** a placa ou o driver não tem mesh shaders. No Mesa, atualize o RADV; na NVIDIA,
  use o driver proprietário.
- **vkd3d-proton antigo ou o d3d12 do próprio Wine:** o Wine traz um `d3d12` que não tem o que o Spark precisa. Se
  o `preparar` disser que falta o vkd3d-proton, instale a versão mais nova no prefixo como descrito acima.
- **Quadros longos e falta de memória:** confira `vm.max_map_count` (o `preparar` avisa) e a RAM livre.

## Problemas só no Linux

Abra a issue com o rótulo `linux` e o rótulo da placa (`nvidia`, `amd` ou `intel`). Cole a saída do
`scripts/linux/spark.sh diagnostico` e diga se o mesmo título e o mesmo commit funcionam no Windows. Valem as regras de
sempre: nada de log, captura, caminho pessoal ou nome de conteúdo ([asset-policy.md](asset-policy.md)).

## AutoPilot no Linux

`python3 scripts/autopilot.py rodar <nome>` funciona igual ao Windows, com estas diferenças:

- usa o build `target/x86_64-pc-windows-msvc/diag/victory.exe` (`scripts/linux/spark.sh compilar diag`) e roda pelo
  `spark.sh`, no mesmo prefixo;
- logs em `~/.local/state/vitoria-spark/execucoes`;
- sem som: durante a execução o prefixo fica sem driver de áudio (`HKCU\Software\Wine\Drivers`, `Audio` vazio), e o
  valor é apagado no fim. O `pycaw` é só do Windows. Ainda não verificado se o título aceita rodar sem dispositivo
  de áudio;
- para encerrar, usa `wineserver -k` no prefixo dedicado. Não use esse prefixo para outra coisa durante o AutoPilot;
- a janela fica fora da tela (`VICTORY_HIDDEN_WINDOW=1`), mas a execução precisa de uma sessão gráfica (X11 ou
  Wayland com XWayland). Rodar sem monitor ainda não foi verificado.
