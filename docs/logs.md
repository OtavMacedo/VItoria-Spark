# Logs da sessão (para pessoas e para IAs)

Cada `victory run` cria uma pasta `logs/<hora UTC>-<pid>/` (relativa à pasta
de onde o executor foi aberto; `SPARK_LOG_DIR=<pasta>` muda a base). O log de
texto do console continua igual; a pasta é um extra feito para ser lido por
máquina e por assistentes de IA com contexto limitado.

| Arquivo | O que é |
|---|---|
| `resumo.md` | Resumo curto (< 150 linhas): números da sessão e **Próximo alvo**. Comece por ele. |
| `resumo.json` | Os mesmos números, em JSON (para scripts e comparações entre execuções). |
| `eventos.jsonl` | Um objeto JSON por linha: a 1ª linha é a sessão, as demais são os eventos do log. |

Quem lê tudo isso por você é o motor de diagnóstico ([motor.md](motor.md)): `python scripts/motor.py diagnosticar
<pasta>` devolve todos os problemas da execução em ordem, com evidência e correção sugerida (só imprime; com
`--gravar` deixa `diagnostico.md` e `plano.toml` na pasta).

`resumo.*` é reescrito a cada ~60 s e no fim (saída do título, `abort`,
falha, perda do dispositivo), então mesmo um crash deixa um resumo.

## Privacidade

Em todos os arquivos, a pasta do usuário vira `<HOME>`, a pasta do jogo
`<JOGO>`, o nome do usuário `<USUARIO>`, o do computador `<PC>` e e-mails
`<EMAIL>`. Mesmo assim, `eventos.jsonl` traz as mensagens de depuração do
próprio título (target `title`): antes de publicar o arquivo, filtre esse
target. Os logs nunca vão para o repositório (`/logs/` está no `.gitignore`;
ver `docs/asset-policy.md`).

## `eventos.jsonl`

Primeira linha (`"kind":"session"`): `version`, `git`, `profile`
(release/diag), `os`, `cpu`, `logical_cpus`, `ram_mb`, `env` (todas as
variáveis `VICTORY_*` e `SPARK_*`). A GPU (nome, driver, VRAM, largura de
onda) vem no evento com `spark.kind = "adapter"`, escrito quando o
dispositivo é criado.

Demais linhas:

```json
{"ts":"2026-11-26T15:30:12.345Z","t":12.345,"level":"WARN","target":"victory_gpu::pipeline::host","thread":"…","message":"host pipeline refused","fields":{"spark.kind":"pipeline_refused","error":"…","shaders":"[…]"}}
```

- `t`: segundos desde o início da sessão.
- `fields`: os campos estruturados do evento. Os `spark.*` existem só aqui
  (o console não os mostra). `spark.kind` marca o que o resumo lê:

| `spark.kind` | Campos |
|---|---|
| `frame_rate` (a cada 5 s) | `spark.fps`, `spark.ms`, `spark.preparing_ms`, `spark.copies_ms`/`_mb`, `spark.textures_ms`/`_mb`, `spark.gpu_ms`, `spark.copy_back_ms`/`_mb`, `spark.draws`, `spark.dropped`, `spark.indirect`, `spark.making` |
| `frame_phases` (a cada 5 s) | média por quadro de cada fase: `spark.preparing_ms`, `spark.copies_ms`, `spark.write_watch_ms`, `spark.textures_ms`, `spark.gpu_ms`, `spark.copy_back_ms`, `spark.present_ms`, `spark.remap_ms`, `spark.pipeline_wait_ms`, `spark.card_idle_ms`, `spark.disk_ms`, `spark.pace_ms`; e `spark.frames`, `spark.copies_mb`, `spark.copy_back_mb`, `spark.watch_calls`, `spark.watch_pages` |
| `draws_dropped` (a cada 5 s, se houve) | por quadro, por causa: `spark.being_made`, `spark.untranslated`, `spark.refused`, `spark.no_pipeline`, `spark.builder_off`, `spark.unbound`, `spark.skipped`, `spark.other` |
| `video_memory` | `spark.vram_used_mb`, `spark.vram_budget_mb`, `spark.shared_used_mb` |
| `adapter` | `spark.name`, `spark.vendor`, `spark.driver`, `spark.vram_mb`, `spark.wave_max`, `spark.create_ms` (do pedido do título ao dispositivo pronto) |
| `driver_cache` (uma vez) | `spark.mb`, `spark.files`: o cache de shaders do driver no início |
| `pipeline_refused` | `shaders`, `error`, `cause` |
| `shader_untranslated` | `hash` (16 dígitos do BLAKE3 do contêiner), `stage`, `error`, `spark.instruction`, `spark.bytes` |
| `device_lost` | `spark.reason` |

Falhas (exceção não tratada, panic, falha da GPU) aparecem como target
`spark::crash` com `code`, `location`, `detail` e `stack`.

Custo: os eventos são serializados na thread que os emite e gravados por uma
thread própria (`spark-log`) através de uma fila limitada; com a fila cheia o
evento é descartado e contado (`events_dropped` no resumo), nunca esperado.
Um mesmo ponto do código que emite mais de 30 eventos em 10 s tem o excesso
contado e resumido numa linha `repeated events counted instead of written`.
Não há eventos por draw; por quadro há só somas em memória: o comprimento do quadro e, com `SPARK_PHASES=1`, 16
leituras de contadores atômicos e uma dúzia de somas sob a mesma trava (menos de 1 µs por quadro no teste
`a_frame_costs_microseconds`), mais duas leituras de relógio por consulta ao write watch, que custa dezenas de
microssegundos. As fases ficam desligadas por padrão enquanto um A/B na execução não mostrar, com folga, que não
custam nada.

### Variáveis

| Variável | Efeito |
|---|---|
| `SPARK_LOG_DIR=<pasta>` | base das pastas de sessão (padrão: `logs`) |
| `SPARK_LOG_JSON=0` | não grava `eventos.jsonl` (o resumo continua) |
| `SPARK_LOG_JSON_LEVEL=warn\|info\|debug\|trace` | nível mínimo gravado no `eventos.jsonl` (padrão `info`; debug/trace só em build diag) |
| `SPARK_GIT_HASH=<hash>` | (na compilação) o commit que a 1ª linha informa |
| `SPARK_STABLE_DRAWS=<n>` | desenhos por quadro a partir dos quais a cena é a estável (padrão 4500) |
| `SPARK_STABLE_AFTER_S=<s>` | segundos depois do 1º quadro do mundo antes da cena estável (padrão 60) |
| `SPARK_AB=CHAVE[=a/b][+CHAVE2...][*CHAVE3...][:s]` | A/B dentro da execução; `*` separa fatores de um fatorial (ver abaixo) |
| `SPARK_PHASES=1` | mede as fases de cada quadro (padrão: não mede; `autopilot.py rodar` liga). Pode alternar numa execução: `SPARK_AB=SPARK_PHASES` mede o custo da própria medição |
| `SPARK_OFFLINE=1` | guarda em `offline/` os contêineres dos shaders que não traduzem e das pipelines recusadas, para `motor.py reproduzir` (padrão: não guarda) |
| `SPARK_PLAN=<plano.toml>` | vários A/B em sequência na mesma execução (ver abaixo) |

`VICTORY_LOG` continua filtrando só o console.

## `resumo.md` / `resumo.json`

- **Visão geral**: duração, tempo até o 1º quadro do mundo (10 quadros
  seguidos com 6+ submissões ao host), motivo do fim.
- **Quadros**: p50/p95/p99/máx em ms, da cena estável, do mundo e de todos;
  quadros > 1 s. A **cena estável** começa na primeira janela de 1 s do
  medidor com 4.500+ desenhos por quadro, 60 s ou mais depois do 1º quadro do
  mundo: o carregamento e a cena meio carregada (800–3.000 desenhos) ficam
  fora. Compare execuções por ela.
- **Composição do quadro**: medianas das linhas de fps (só mundo, se houver)
  de preparando, cópias, texturas, GPU, cópia de volta, draws e descartados;
  e as mesmas medianas de **todas** as janelas de 1 s da cena estável (5 vezes
  mais amostras que as linhas de 5 s).
- **Descartados por causa** (`dropped_by_cause`): por quadro, quantos desenhos não rodaram por pipeline sendo
  feita, shader que não traduz, pipeline recusada, falta de pipeline, recursos não ligados; e, à parte
  (`on_purpose`), o que os ajustes deixam de fora de propósito (o construtor de estruturas de ray tracing desligado,
  uma chave de diagnóstico). Só os primeiros são falha.
- **Fases do quadro** (`phases`, `phases_world`): por fase, média, p50, p95, p99 e máximo em ms por quadro, e a
  parte do quadro, medidos em **cada quadro** do mundo (a cena estável, quando há). As fases: `preparing`
  (preparo das submissões), dentro dele `copies` e `write_watch`, `textures` (todas as threads), `gpu` (espera pela
  placa), `copy_back`, `present` (a chamada de apresentação do título), `remap` (o runtime trocando o que a memória
  do título mostra), `pipeline_wait`, `card_idle`, `disk` e `pace`. Elas se sobrepõem e não somam o quadro. Junto:
  MB por quadro e MB/s das cópias e da cópia de volta, consultas ao write watch por quadro, páginas e µs por
  consulta.
- **Quadros de mais de 1 s por causa** (`long_frame_causes`): cada quadro longo vai para a maior entre preparo,
  placa, cópia de volta, remapeamento e espera por pipelines, se ela passa de 25 % do quadro; senão para
  `page_faults` (o amostrador viu mais de 5.000 falhas de página/s) ou `unexplained`.
- **Shaders que não traduzem** (`untranslated_shaders`): até 64, com `hash`, `stage` e a instrução.
- **Medição** (`measurement`): `device_create_ms`, `driver_cache_mb`, `driver_cache_files`; e, na cena estável,
  `windows_making_pipelines` (janelas com pipelines ainda sendo feitas: caches frios).
- **A/B na rodada** (só com `SPARK_AB`): ver abaixo.
- **Pipelines recusadas** e **Instruções não traduzidas**: top 10 com
  contagem (por lista de shaders / por instrução, com nº de shaders).
- **Dispositivo perdido / TDR**, **Falha** (código, módulo+offset, pilha).
- **Memória**: RAM livre mínima, falhas de página/s, VRAM (uso máximo,
  orçamento mínimo, compartilhada). `own_allocations_counted` (no `resumo.json`) diz se as alocações do nosso
  código foram contadas: com `VICTORY_COUNT_ALLOCS=0` o alocador só repassa cada chamada (sem os contadores, que
  custam em toda alocação do processo), o `resumo.md` ganha a linha "Alocações nossas: não contadas" e as linhas
  `memory, what holds it` / `memory, more` do log dizem `not counted` no lugar dos números. A chave é lida uma vez
  no início do processo e não alterna no `SPARK_AB` (um bloco contado ao nascer e não ao morrer deixaria os totais
  errados): compara-se com execuções separadas.
- **Avisos e erros**: top 10 deduplicados (números trocados por `#`).
- **Próximo alvo**: regras simples sobre os números acima (o diagnóstico completo é o do motor), em ordem:
  falha → dispositivo perdido → não chegou ao mundo → draws descartados
  (com a instrução que mais falta) → memória (quadros > 1 s com muitas
  falhas de página ou pouca RAM) → engasgos → VRAM no limite → placa vs.
  CPU (GPU > preparação: "placa é o limite"; senão o maior pedaço da
  preparação).

## A/B dentro da execução (`SPARK_AB`)

Duas execuções do mesmo build diferem mais do que a maioria das mudanças vale
(cena, memória livre, estado da placa). Com `SPARK_AB` uma execução só mede a
opção dos dois jeitos, em blocos alternados da mesma cena:

- `SPARK_AB=VICTORY_X` alterna `VICTORY_X` entre `0` (braço A) e `1` (braço
  B) em blocos de 20 s; `SPARK_AB=VICTORY_X=80/94:30` alterna entre 80 e 94 em
  blocos de 30 s; `+` junta chaves que mudam juntas. `VICTORY_X=alt` é o mesmo
  que `SPARK_AB=VICTORY_X`.
- A troca acontece na apresentação de um quadro. Os blocos seguem a sequência
  de Thue–Morse (A B B A B A A B…), para que uma deriva lenta da cena caia nos
  dois braços, e o primeiro décimo de cada bloco depois de uma troca (mín. 1 s)
  fica fora das contas. Só contam quadros da cena estável.
- Só alternam as chaves que decidem a cada uso, sem estado do valor antigo:
  `VICTORY_CARD_SYNC_BATCH`, `VICTORY_COPY_PLAIN`, `VICTORY_MERGE_MASK`,
  `VICTORY_SKIP_WC_BACK` (mede tempo, não imagem: o que ficou sem voltar num
  braço continua assim no outro), `VICTORY_WC_NO_BACK` (idem; o piso
  `VICTORY_WC_NO_BACK_MIN_KB` vale nos dois braços), `VICTORY_STEP_INDEX`,
  `VICTORY_TEXTURE_ROOM_PERCENT`, `VICTORY_WIDE_MERGE`, `VICTORY_WIDE_REFRESH`, `VICTORY_WIDE_THREADS`
  (com dois números: `SPARK_AB=VICTORY_WIDE_THREADS=4/8`), `VICTORY_WIDE_WAKE_ONE`, `VICTORY_LIST_REAPER`, `VICTORY_NOTHING_NEW_FAST` e `VICTORY_FAULT_UNLOCKED`. Outra chave em
  `SPARK_AB` recusa a execução na hora, com a lista das que podem; e também uma chave que ficaria sem efeito pela
  configuração (ex.: `VICTORY_MERGE_MASK` com o bit 2 de `VICTORY_QUEUE2`), pois a execução não mediria nada.
- As capturas (`VICTORY_CAPTURE`) levam o braço no nome (`frame-N-a.png`, `frame-N-b.png`), para comparar a
  imagem braço a braço.
- O resumo ganha a seção **A/B na rodada**: por braço, quadros, p50/p95/p99,
  média, quadros > 1 s e as medianas das partes do quadro das janelas de 1 s
  inteiras num braço; e o veredito pela diferença pareada das medianas de
  blocos vizinhos (um de cada braço), com intervalo de confiança de 95 %
  (t de Student). `resumo.json` traz o mesmo em `ab`, mais os números de cada bloco (`block_stats`: índice, braço,
  quadros, p50, p95, média, quadros > 1 s), que é o que o motor (`scripts/motor.py ab`) analisa
  (escala log, tendência linear, parada sequencial).

### Várias chaves ao mesmo tempo (fatorial)

`SPARK_AB=VICTORY_X*VICTORY_Y*VICTORY_Z` faz de cada lado do `*` um fator (até 5; `+` continua juntando chaves que
mudam juntas). Cada bloco roda uma combinação dos níveis, de modo que todo bloco informa todos os fatores: três
chaves são julgadas numa execução pouco mais longa que a de uma. Nenhum fator fica mais de 4 blocos seguidos no
mesmo nível (a regra dos níveis está em [motor.md](motor.md)). O `ab` do `resumo.json` ganha `factors` (cada fator
como `CHAVE=a/b`) e cada bloco de `block_stats` ganha `cell` (bit `f` = nível do fator `f`; `arm` continua sendo o
nível do fator 0). As capturas viram `frame-N-c<célula>.png`. A seção "A/B na rodada" do `resumo.md` mostra só os
dois níveis do 1º fator; o efeito de cada fator, com intervalo e interações, sai de `python scripts/motor.py ab
<pasta>` ([motor.md](motor.md)).

### Vários experimentos numa execução (`SPARK_PLAN`)

A partida do título (menus, carregamento, cena estável) custa minutos; um plano a paga uma vez para vários A/B:

```toml
periodo_s = 20
[[experimento]]
nome = "merge"
ab = "VICTORY_WIDE_MERGE"
[[experimento]]
nome = "aa"
ab = "VICTORY_STEP_INDEX=0/0"
```

O experimento em vigor é o número escrito em `<plano.toml>.atual` (0 se o arquivo não existe), lido a cada segundo:
quem acompanha a execução (`scripts/amostras.py`, usado por `scripts/autopilot.py rodar`) escreve o seguinte quando
o atual decide. Cada experimento novo começa com um bloco de lavagem no braço A, fora das contas. O resumo traz um
`ab` por experimento em `ab_experiments` e uma seção "A/B na rodada" para cada um; `python scripts/motor.py ab
<pasta> --exp N` julga o experimento N. Um experimento pode ser fatorial (`ab = "VICTORY_X*VICTORY_Y"`); o plano
que `motor.py diagnosticar --gravar` grava (`plano.toml`) junta assim todas as chaves candidatas da execução.

## Fluxo recomendado com uma IA

1. Rode o jogo e feche normalmente (ou deixe cair).
2. Rode `python scripts/motor.py diagnosticar <pasta> --gravar` e entregue o `diagnostico.md` (ou **só o `resumo.md`**),
   com algo como:

   > Este é o resumo de uma execução do VItoria Spark (camada Xbox Series →
   > Windows/D3D12). Leia "Próximo alvo" e me diga o que investigar no
   > código e quais linhas do `eventos.jsonl` você precisa ver.

3. Quando a IA pedir detalhes, filtre o `eventos.jsonl` por `target`,
   `level` ou `spark.kind` em vez de mandar o arquivo inteiro:

   ```powershell
   # avisos e erros
   Select-String -Path eventos.jsonl -Pattern '"level":"(WARN|ERROR)"'
   # só as linhas de fps
   Select-String -Path eventos.jsonl -Pattern '"spark.kind":"frame_rate"'
   ```

   ```sh
   jq -c 'select(.target | startswith("victory_gpu::pipeline"))' eventos.jsonl
   ```

4. Para comparar duas execuções, entregue os dois `resumo.json`.

## Exemplo

Um resumo de uma execução sintética (sem o jogo) sai de
`cargo test --release -p victory-core example_summary -- --nocapture`.
Trecho:

```markdown
## Próximo alvo

1. **Draws descartados**: 40 por quadro sem pipeline do host (de 2400). Corrigir as pipelines e shaders das tabelas "Pipelines recusadas" e "Instruções não traduzidas"; a instrução mais frequente é `v_permlane16_b32`.
2. **Memória**: 5 quadro(s) de mais de 1 s no mundo com muitas falhas de página e pouca RAM livre; ...
3. **VRAM no limite**: uso de até 7900 MB para orçamento de 7400 MB; ...
4. **Placa é o limite**: 24.0 ms/quadro na GPU contra 18.0 ms preparando. ...

## Quadros

| quadros | n | p50 ms | p95 ms | p99 ms | máx ms | fps (p50) |
|---|---|---|---|---|---|---|
| mundo | 1622 | 32.0 | 40.0 | 40.0 | 1500.0 | 31.2 |
| todos | 2221 | 33.4 | 39.0 | 40.0 | 1500.0 | 30.0 |
```
