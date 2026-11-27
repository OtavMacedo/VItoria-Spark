# Motor de diagnóstico

Uma execução do título custa minutos. O motor existe para que **uma** execução renda o máximo: todos os problemas
que ela mostra, em ordem, cada um com evidência e correção sugerida; várias chaves julgadas na mesma execução; e as
falhas de shaders e pipelines guardadas para serem verificadas depois **sem** o título.

```text
python scripts/motor.py diagnosticar <sessão>     # todos os problemas, em ordem, e o plano da próxima execução
python scripts/motor.py plano <sessão>            # só o plano (SPARK_PLAN fatorial + itens offline)
python scripts/motor.py ab <sessão> [--exp N]     # veredito do A/B da execução (um por fator, num fatorial)
python scripts/motor.py comparar --a <sessões> --b <sessões>   # veredito entre execuções + fases que mudaram
python scripts/motor.py reproduzir <sessão>       # sem o título: retraduz e cria as pipelines das falhas guardadas
python scripts/motor.py ruido <sessão A/A> --gravar <ruido.json>
python scripts/motor.py consultar <decisoes.jsonl> [<CHAVE>]
```

`<sessão>` é a pasta de uma execução (vale o `resumo.json` mais novo dentro dela). Tudo o que o motor grava
(`diagnostico.md`, `diagnostico.json`, `plano.toml`, `plano.md`, `veredito*.md`, `reproducao.md`) fica na pasta da
sessão, fora do repositório. `scripts/veredito.py` e `scripts/ab_estat.py` continuam funcionando: chamam o motor.

## O ciclo com o motor

1. **Uma execução de base** (`python scripts/autopilot.py rodar base-1`): no fim sai o diagnóstico.
2. **Ler `diagnostico.md`**: primeiro a seção *Medição*. Se o estado é `PROBLEMA_DE_MEDICAO`, os números não valem:
   siga a ação indicada e repita. Senão, a lista de problemas já diz o que atacar, com que chave e em que arquivo.
3. **Uma execução para todas as chaves** (`SPARK_PLAN=<sessão>/plano.toml`): o plano põe as chaves candidatas como
   fatores de um A/B fatorial; `motor.py ab <pasta> --exp N` dá um veredito por chave.
4. **Código sem o título**: para shaders que não traduzem e pipelines recusadas, mude o tradutor e rode
   `motor.py reproduzir <sessão>` até os itens ficarem `CORRIGIDO`.
5. **Uma execução de confirmação** no fim do lote (depois de `aquecer`), comparada com a base.

## Diagnóstico (`diagnosticar`)

O estado é um de `OK`, `PROBLEMA_DE_MEDICAO` (a medição não vale) e `FALHA` (falha, placa perdida ou não chegou ao
mundo). Cada problema traz: evidência (ms por quadro, % do quadro, percentis, MB, contagens, hashes), correção
sugerida, chaves a testar, arquivos do código e quantos itens dá para verificar sem o título.

| Problema | De onde vem | Ordem (`pontos`) |
|---|---|---|
| `falha`, `dispositivo_perdido`, `sem_mundo` | `crash`, `device_lost`, `first_world_frame_s` | 800 a 1000 |
| `fase:<nome>` (placa, cópias, write watch, texturas, cópia de volta, remapeamento, espera por pipelines, resto do preparo) | `phases` do resumo | a parte do quadro, em % |
| `quadros_longos` | `long_frame_causes`, memória | 2 × a parte do tempo em quadros > 1 s |
| `shaders_nao_traduzidos` | `untranslated_instructions`, `untranslated_shaders` | 30 a 60 |
| `pipelines_recusadas` | `refused_pipelines` | 25 a 50 |
| `descartes` (só os que são falha) | `dropped_by_cause` | 20 a 60 |
| `memoria` | VRAM contra o orçamento, memória compartilhada | 25 |
| `imagem` | avisos (quadro preto, textura reenviada, vista nula...) | 15 a 30 |
| `descartes_de_proposito` | `dropped_by_cause` (`on_purpose`) | 1 (informação) |

As fases se sobrepõem (cópias, write watch e texturas estão dentro do preparo; threads diferentes correm ao mesmo
tempo), então as partes não somam 100 %: cada uma é lida contra o quadro.

Execuções anteriores às fases por quadro também são diagnosticadas: as fases vêm das medianas das janelas de 1 s e,
num build de diagnóstico, das mensagens do log de quadros; o relatório diz isso e não mostra percentis.

### Problemas de medição

| Achado | Regra | Grave? |
|---|---|---|
| `dispositivo_lento` | criação do dispositivo D3D12 acima de 30 s | sim |
| `cache_do_driver` | cache de shaders do driver acima de 4 GB no início | sim |
| `sem_mundo`, `sem_cena_estavel`, `poucas_amostras` | menos de 20 janelas de 1 s da cena estável | sim |
| `caches_frios` | mais de 20 % das amostras estáveis com pipelines sendo feitas | sim |
| `tempestade_de_faltas` | quadros > 1 s com 5.000+ falhas de página/s e 3× a média | se passam de 10 % do tempo |
| `ram_baixa` | RAM livre mínima abaixo de 1.024 MB | abaixo de 512 MB |
| `cena_instavel` | IC 95 % da mediana do quadro acima de ±10 % | não |
| `ab_na_execucao` | a execução alternou chaves: as fases misturam os braços | não |
| `eventos_perdidos` | fila do log cheia | não |

Num A/B na execução os dois braços dividem a máquina, então estes achados entram como alerta no veredito. Entre
execuções (`comparar`), um achado grave em qualquer execução dá `PROBLEMA_DE_MEDICAO` (`--sem-medicao` desliga).

## Várias chaves numa execução (fatorial)

`SPARK_AB=A*B*C` (ou `ab = "A*B*C"` num experimento de `SPARK_PLAN`) faz de cada lado do `*` um fator; `+` continua
juntando chaves que mudam juntas (`A+B*C` tem dois fatores). Cada bloco roda uma combinação de níveis:

- o nível do fator `f` no bloco `b` é o braço Thue–Morse de `b >> f`: o fator 0 alterna como no A/B simples, e todo
  trecho de `2^k` blocos alinhados tem cada combinação uma vez (fatores equilibrados entre si e contra a deriva);
- **todo bloco informa todos os fatores**: N chaves custam pouco mais que uma, em vez de N execuções;
- o motor ajusta `ln(mediana do bloco) = a + b·índice + Σ efeito_f·nível_f` e dá, por fator, efeito, IC 95 %, t e
  estado com os mesmos critérios do A/B (p95 e quadros > 1 s por nível);
- um segundo ajuste com os produtos dois a dois acusa **interação** (|t| ≥ 3): os efeitos não se somam, e o fator
  não é GANHO sozinho (teste os dois juntos num A/B simples);
- a execução para quando **todos** os fatores decidiram; o teto cresce 240 s por fator a mais. Até 5 fatores.

As capturas levam a combinação no nome (`frame-N-c<célula>.png`, bit `f` = nível do fator `f`). Só entram as chaves
da tabela `crates/victory-gpu/src/switches.rs`; o plano as tira de lá e deixa fora as que a configuração tornaria
sem efeito.

## Verificação sem o título (`reproduzir`)

Com `SPARK_OFFLINE=1` (desligado por padrão; `autopilot.py rodar` liga), a execução grava em `<sessão>/offline/` o
contêiner de cada shader que não traduziu ou cuja pipeline o host recusou (`<hash>.<estágio>.cdxil`) e uma linha
por falha em `manifesto.jsonl`. São dados do título: ficam na pasta da sessão e **nunca** entram no repositório.
Limites por execução: 512 contêineres, 256 MB.

`motor.py reproduzir <sessão>` roda `victory translate <offline> --pipelines --list` com a largura de onda e o
limite de vistas que a execução teve, e compara cada item com o erro de antes: `CORRIGIDO`, `AINDA_FALHA`,
`SEM_ARQUIVO`. Passe as chaves do tradutor que quiser testar como `CHAVE=valor`.

Limite conhecido: o `translate` cria a pipeline de cada shader com um estado gráfico genérico. Recusas de compute
são reproduzidas por inteiro; uma recusa que dependa do estado gráfico do título só se confirma na execução.

## Módulos (`scripts/motor/`)

| Módulo | Papel |
|---|---|
| `sessao.py` | lê `resumo.json` e, numa passada, o `eventos.jsonl`; único lugar que abre arquivos de log |
| `estat.py` | regressão por blocos, A/B, fatorial, piso de ruído, entre execuções, Fisher |
| `medicao.py` | problemas de medição (limites no topo do arquivo) |
| `problemas.py` | o diagnóstico: todos os problemas, em ordem |
| `conhecimento.py` | por fase: código, chaves e ideia de correção (a única tabela a editar) |
| `plano.py` | a próxima execução: plano fatorial, chaves de execução própria, itens offline |
| `veredito.py` | os critérios de GANHO/REGRESSAO para A/B, fatorial e entre execuções |
| `offline.py` | a verificação sem o título |
| `relatorio.py`, `cli.py` | o texto e os comandos |

Testes (dados sintéticos): `python -m unittest discover -s scripts/tests`. Eles também conferem que todo arquivo e
toda chave citados em `conhecimento.py` existem no repositório.
