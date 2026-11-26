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
| `video_memory` | `spark.vram_used_mb`, `spark.vram_budget_mb`, `spark.shared_used_mb` |
| `adapter` | `spark.name`, `spark.vendor`, `spark.driver`, `spark.vram_mb`, `spark.wave_max` |
| `pipeline_refused` | `shaders`, `error` |
| `shader_untranslated` | `hash`, `stage`, `error`, `spark.instruction` |
| `device_lost` | `spark.reason` |

Falhas (exceção não tratada, panic, falha da GPU) aparecem como target
`spark::crash` com `code`, `location`, `detail` e `stack`.

Custo: os eventos são serializados na thread que os emite e gravados por uma
thread própria (`spark-log`) através de uma fila limitada; com a fila cheia o
evento é descartado e contado (`events_dropped` no resumo), nunca esperado.
Um mesmo ponto do código que emite mais de 30 eventos em 10 s tem o excesso
contado e resumido numa linha `repeated events counted instead of written`.
Não há eventos por draw; por quadro há só uma soma em memória.

### Variáveis

| Variável | Efeito |
|---|---|
| `SPARK_LOG_DIR=<pasta>` | base das pastas de sessão (padrão: `logs`) |
| `SPARK_LOG_JSON=0` | não grava `eventos.jsonl` (o resumo continua) |
| `SPARK_LOG_JSON_LEVEL=warn\|info\|debug\|trace` | nível mínimo gravado no `eventos.jsonl` (padrão `info`; debug/trace só em build diag) |
| `SPARK_GIT_HASH=<hash>` | (na compilação) o commit que a 1ª linha informa |

`VICTORY_LOG` continua filtrando só o console.

## `resumo.md` / `resumo.json`

- **Visão geral**: duração, tempo até o 1º quadro do mundo (10 quadros
  seguidos com 6+ submissões ao host), motivo do fim.
- **Quadros**: p50/p95/p99/máx em ms, do mundo e de todos; quadros > 1 s.
- **Composição do quadro**: medianas das linhas de fps (só mundo, se houver)
  de preparando, cópias, texturas, GPU, cópia de volta, draws e descartados.
- **Pipelines recusadas** e **Instruções não traduzidas**: top 10 com
  contagem (por lista de shaders / por instrução, com nº de shaders).
- **Dispositivo perdido / TDR**, **Falha** (código, módulo+offset, pilha).
- **Memória**: RAM livre mínima, falhas de página/s, VRAM (uso máximo,
  orçamento mínimo, compartilhada).
- **Avisos e erros**: top 10 deduplicados (números trocados por `#`).
- **Próximo alvo**: regras simples sobre os números acima, em ordem:
  falha → dispositivo perdido → não chegou ao mundo → draws descartados
  (com a instrução que mais falta) → memória (quadros > 1 s com muitas
  falhas de página ou pouca RAM) → engasgos → VRAM no limite → placa vs.
  CPU (GPU > preparação: "placa é o limite"; senão o maior pedaço da
  preparação).

## Fluxo recomendado com uma IA

1. Rode o jogo e feche normalmente (ou deixe cair).
2. Entregue **só o `resumo.md`** primeiro, com algo como:

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
