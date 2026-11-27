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

`<sessão>` é a pasta de uma execução (vale o `resumo.json` mais novo dentro dela). **Ler não grava:** os comandos
só imprimem, e a pasta de uma execução fica como estava. Com `--gravar`, `diagnosticar`, `plano`, `ab`, `comparar` e
`reproduzir` deixam o que imprimiram na pasta da sessão (`diagnostico.md`, `diagnostico.json`, `plano.toml`,
`plano.md`, `veredito*.md`, `reproducao.md`), nunca dentro do repositório; `autopilot.py rodar` grava assim os da
execução que acabou de fazer. `--registrar` e `ruido --gravar` recusam um destino dentro do repositório.
`scripts/veredito.py` e `scripts/ab_estat.py` continuam funcionando como antes: chamam o motor.

## O ciclo com o motor

1. **Uma execução de base** (`python scripts/autopilot.py rodar base-1`): no fim sai o diagnóstico.
2. **Ler `diagnostico.md`**: primeiro a seção *Medição*. Se o estado é `PROBLEMA_DE_MEDICAO`, os números não valem:
   siga a ação indicada e repita. Senão, a lista de problemas já diz o que atacar, com que chave e em que arquivo.
3. **Uma execução para todas as chaves** (`SPARK_PLAN=<sessão>/plano.toml`; `motor.py plano <sessão> --gravar` o
   grava quando a execução não veio do `autopilot.py rodar`): o plano põe as chaves candidatas como fatores de um
   A/B fatorial; `motor.py ab <pasta> --exp N` dá um veredito por chave.
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

As fases por quadro pedem `SPARK_PHASES=1` (desligado por padrão; `autopilot.py rodar` liga). Execuções sem elas
e as anteriores a elas também são diagnosticadas: as fases vêm das medianas das janelas de 1 s e,
num build de diagnóstico, das mensagens do log de quadros; o relatório diz isso e não mostra percentis.

### Problemas de medição

| Achado | Regra | Grave? |
|---|---|---|
| `dispositivo_lento` | criação do dispositivo D3D12 acima de 30 s | sim |
| `cache_do_driver` | cache de shaders do driver acima de 4 GB no início | não: o que invalida é o efeito medido (`dispositivo_lento`) |
| `sem_mundo`, `sem_cena_estavel`, `poucas_amostras` | menos de 20 janelas de 1 s da cena estável | sim |
| `caches_frios` | mais de 20 % das amostras estáveis com pipelines sendo feitas | sim |
| `tempestade_de_faltas` | quadros > 1 s com 5.000+ falhas de página/s e 3× a média | se passam de 10 % do tempo |
| `ram_baixa` | RAM livre mínima abaixo de 1.024 MB | abaixo de 512 MB |
| `cena_instavel` | IC 95 % da mediana do quadro acima de ±10 % | não |
| `ab_na_execucao` | a execução alternou chaves: as fases misturam os braços | não |
| `eventos_perdidos` | fila do log cheia | não |

O estado `PROBLEMA_DE_MEDICAO` vem só dos achados graves. A espera pela placa é dita "placa" no cabeçalho só
quando a placa trabalha durante ela: com a placa ociosa em mais de 30 % da espera, o limite dito é a sincronização.

Num A/B na execução os dois braços dividem a máquina, então estes achados entram como alerta no veredito. Entre
execuções (`comparar`), um achado grave em qualquer execução dá `PROBLEMA_DE_MEDICAO` (`--sem-medicao` desliga).

## Várias chaves numa execução (fatorial)

`SPARK_AB=A*B*C` (ou `ab = "A*B*C"` num experimento de `SPARK_PLAN`) faz de cada lado do `*` um fator; `+` continua
juntando chaves que mudam juntas (`A+B*C` tem dois fatores). Cada bloco roda uma combinação de níveis:

- o nível do fator `f` no bloco `b` é a paridade de alguns bits de `b`: todos para o fator 0 (a sequência de
  Thue–Morse do A/B simples), todos menos o bit 0 para o fator 1 e, daí em diante, todos menos os bits 0 e `f`.
  Assim os blocos `2j` e `2j+1` só diferem no fator 0, todo trecho de `2^k` blocos alinhados tem cada combinação
  uma vez (fatores equilibrados entre si) e **nenhum fator fica mais de 4 blocos seguidos no mesmo nível**: uma
  deriva lenta da cena não passa por efeito de um fator;
- **todo bloco informa todos os fatores**: N chaves custam pouco mais que uma, em vez de N execuções;
- o motor ajusta `ln(mediana do bloco) = a + b·índice + Σ efeito_f·nível_f` e dá, por fator, efeito, IC 95 %, t e
  estado com os mesmos critérios do A/B (p95 e quadros > 1 s por nível);
- **k fatores são k comparações**: cada fator decide com o |t| cujo p é o de |t| = 3 dividido por k (Bonferroni;
  com 20 graus de liberdade, 3,3 para dois fatores e 3,7 para cinco), `SEM_EFEITO` pede o intervalo simultâneo
  dentro de ±2 %, e o teste de quadros > 1 s de cada fator usa 5 %/k. A chance de algum fator sem efeito sair
  `GANHO` ou `REGRESSAO` não cresce com o número de fatores;
- um segundo ajuste com os produtos dois a dois acusa **interação** (|t| ≥ 3): os efeitos não se somam, e o fator
  não é GANHO sozinho (teste os dois juntos num A/B simples). Com blocos de menos para esse ajuste (cinco fatores
  pedem 19), o veredito diz "interações: não testadas" em vez de dizer que não há;
- a execução para quando **todos** os fatores decidiram; o teto cresce 240 s por fator a mais. Até 5 fatores.

As capturas levam a combinação no nome (`frame-N-c<célula>.png`, bit `f` = nível do fator `f`). Só entram as chaves
da tabela `crates/victory-gpu/src/switches.rs`; o plano as tira de lá e deixa fora as que a configuração tornaria
sem efeito e as de medição (`SPARK_PHASES`, que está na tabela para o seu próprio custo poder ser medido:
`SPARK_AB=SPARK_PHASES=0/1`).

Simulado com o ruído e a deriva das execuções medidas (resíduos por bloco de 1,5 % com autocorrelação de 0,2 a 0,6),
olhando a cada 3 blocos até o teto: com cinco fatores sem efeito, algum saía `GANHO`/`REGRESSAO` em 26 % (sem
deriva) a 52 % (com deriva) das execuções quando cada fator decidia com |t| ≥ 3 e o fator `f` trocava de nível a
cada `2^f` blocos; com a regra e os níveis acima, em 1 a 3 %, e um efeito real de −5 % é achado em 80 a 98 %.

## Verificação sem o título (`reproduzir`)

Com `SPARK_OFFLINE=1` (desligado por padrão; `autopilot.py rodar` liga), a execução grava em `<sessão>/offline/` o
contêiner de cada shader que não traduziu ou cuja pipeline o host recusou (`<hash>.<estágio>.cdxil`) e uma linha
por falha em `manifesto.jsonl`. São dados do título: ficam na pasta da sessão e **nunca** entram no repositório.
Limites por execução: 512 contêineres, 256 MB.

`motor.py reproduzir <sessão>` roda `victory translate <offline> --pipelines --list` com a largura de onda e o
limite de vistas que a execução teve, e compara cada item com o erro de antes. Passe as chaves do tradutor que
quiser testar como `CHAVE=valor`.

| Falha guardada | O que a verificação refaz | Estados possíveis |
|---|---|---|
| shader que não traduz | tudo: a tradução só depende do contêiner e das chaves do tradutor | `CORRIGIDO`, `AINDA_FALHA` |
| pipeline de compute recusada | tudo: um shader só, sem estado gráfico | `CORRIGIDO`, `AINDA_FALHA` |
| pipeline gráfica recusada | cada shader **sozinho**, com um par genérico no outro estágio e um estado gráfico simples, não o do título | `AINDA_FALHA`, `SO_NA_EXECUCAO`, e `CORRIGIDO` só se já falhou aqui fora |

O estado gráfico do título de uma pipeline recusada (mistura, rasterização, profundidade, entradas, formatos dos
alvos) fica guardado no manifesto, campo `state`, para quem investiga; o `translate` ainda não cria a pipeline com
ele. Por isso uma pipeline gráfica que passa na verificação sem nunca ter falhado nela sai `SO_NA_EXECUCAO`: a
recusa dependia do que a verificação não tem, e só uma execução confirma. Para poder dar `CORRIGIDO` a uma recusa
gráfica, rode `reproduzir --gravar` **antes** da correção: o que falhar ali fica lembrado em `reproducao.json`
(`reproduzidos`). `SEM_ARQUIVO` é a falha que passou do limite de contêineres da execução.

A gravação recusa uma pasta de sessão dentro do repositório (`SPARK_LOG_DIR` não definido e o título iniciado de
dentro dele): nenhum contêiner é escrito e o log avisa.

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
