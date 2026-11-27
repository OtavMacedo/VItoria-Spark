# AutoPilot Mode

Aponte um agente de código (Claude Code ou similar) para este repositório e para a pasta do seu título. Ele mede,
encontra o maior gargalo, abre uma issue, implementa, mede de novo e abre um PR sozinho, dentro das regras abaixo.

## Como ligar

1. Clone o repositório e instale Rust, Visual Studio Build Tools, Python 3.11+ e `pip install pycaw` (para o áudio
   mudo). Faça login no GitHub CLI com a sua conta: `gh auth login`. No Linux, no lugar do Build Tools e do
   `pycaw`, siga [linux.md](linux.md) (seção "AutoPilot no Linux").
2. Configure `victory.toml` (a partir de `victory.example.toml`) com `game_dir` apontando para **a sua cópia**, fora do
   repositório.
3. **Você mesmo** roda uma vez `python scripts/autopilot.py confirmar` e lê a declaração. O agente não pode fazer isso
   por você.
4. Abra o agente na pasta do repositório e diga:

   > Leia o CLAUDE.md e entre em AutoPilot Mode. Título em `<pasta do jogo>`.

## O ciclo

1. **Sincronizar:** `git fetch`, rebase na `main` e leitura das issues abertas com o rótulo `autopilot`, para não
   repetir trabalho de outra pessoa.
2. **Medir:** compilar o build de diagnóstico e rodar `python scripts/autopilot.py rodar base-1`, depois `base-2`. Cada
   execução roda escondida e sem som, mede só a cena estável e para sozinha quando a mediana do quadro já está
   medida com ±3 % (intervalo de 95 %), com 60 amostras da cena estável ou no tempo limite, e gera o `resumo.md` fora
   do repositório.
3. **Escolher o alvo:** `python scripts/motor.py diagnosticar <pasta da execução>` ([motor.md](motor.md)) lista
   **todos** os problemas da execução em ordem de custo, cada um com evidência, correção sugerida, chaves a testar
   e arquivos; `rodar` já o chama no fim e deixa `diagnostico.md` na pasta. Se o estado for `PROBLEMA_DE_MEDICAO`,
   siga a ação indicada e meça de novo antes de qualquer conclusão. Uma execução rende vários alvos: trabalhe a
   lista de cima para baixo sem rodar o título de novo para cada item. O agente procura uma issue
   aberta sobre o mesmo problema. Se existir, comenta nela com os números agregados; se não, abre uma issue nova
   (modelo `autopilot`) e se atribui a ela.

   **Coordenação entre pessoas e agentes:** várias pessoas rodam o AutoPilot ao mesmo tempo, e as issues dizem quem
   trabalha em quê, para um não passar por cima do outro.
   - **Antes de escolher:** `diagnosticar <pasta> --coordenar` marca os problemas que outra sessão já assumiu;
     `python scripts/motor.py coordenar listar` mostra todos os alvos em andamento. Pule os que estão ocupados.
   - **Ao começar:** `coordenar assumir <alvo> --de <pasta da execução>` (ou `--resumo "<problema + número>"`, até
     70 letras; o alvo é o `id` do problema, como `fase:copy_back`, ou um mais fino, como `pipeline:<hash>`). Abre a issue com o rótulo `em-andamento` e um título
     que diz tudo sem abrir: `[em andamento] fase:copy_back: cópia de volta 70 ms/quadro (29 %)`. Se outra
     sessão assumiu o mesmo alvo no mesmo instante, vale a issue mais antiga e a sua se fecha sozinha. Defina
     `SPARK_SESSAO` com um nome por agente quando vários usam a mesma conta.
   - **A cada passo:** `coordenar status <issue> "<o que foi feito, números agregados, próximo passo>"`, com
     `--etapa "A/B"` ou `--etapa "PR aberto"` quando a etapa muda (o começo do título acompanha). O status é
     também o sinal de vida: sem status por 24 h, o alvo fica livre e outra sessão pode retomá-lo (`assumir` faz
     isso e comenta na issue).
   - **Ao terminar:** o PR diz `Fecha #<issue>`. Ao desistir: `coordenar liberar <issue> "<motivo e o que se
     aprendeu>"`, que tira o rótulo e deixa o alvo livre com o histórico para quem vier depois.
4. **Implementar:** branch `autopilot/<assunto>`, uma mudança lógica por commit, testes de unidade, e opção nova
   desligada por padrão quando houver risco.
5. **Provar:** pelo menos duas execuções com a mudança e duas sem, mesmo build, caches aquecidos. Comparar a mediana e
   o p95 do quadro e os componentes. Uma opção que pode alternar dentro da execução ([logs.md](logs.md), seção
   `SPARK_AB`) se prova numa execução só: `python scripts/autopilot.py rodar ab-1 SPARK_AB=VICTORY_X`; ela para quando
   a parada sequencial decide (ver o veredito abaixo). Depois de mudar o tradutor ou uma opção dele, rode antes
   `python scripts/autopilot.py aquecer [CHAVE=valor ...]`: ele traduz em paralelo, antes do jogo, os shaders que as
   execuções anteriores usaram. Mesmo assim, as pipelines novas são compiladas na primeira execução, então não julgue a
   imagem nela.
   **Várias chaves, uma execução:** o `plano.toml` que o diagnóstico grava põe todas as chaves candidatas num
   A/B fatorial (`ab = "VICTORY_X*VICTORY_Y*VICTORY_Z"`): `python scripts/autopilot.py rodar plano-1
   SPARK_PLAN=<pasta>/plano.toml` julga todas de uma vez, com um veredito por chave e o aviso de interação.
   **Correções do tradutor, nenhuma execução:** shaders que não traduzem e pipelines recusadas ficam guardados na
   pasta da sessão (`SPARK_OFFLINE=1`, que `rodar` liga); `python scripts/motor.py reproduzir <pasta>` retraduz e
   cria as pipelines na placa com o build atual e diz o que ficou `CORRIGIDO`. Rode-o uma vez com `--gravar`
   antes de mexer no tradutor: uma pipeline gráfica só pode sair `CORRIGIDO` sem o título se falhou assim antes
   ([motor.md](motor.md)). O título só roda de novo no fim do lote, para confirmar.
   O julgamento é sempre o de `python scripts/motor.py ab <pasta da execução>` (A/B na execução) ou
   `python scripts/motor.py comparar --a <execuções> --b <execuções>` (entre execuções): 20 linhas e um estado só
   (GANHO, SEM_EFEITO, INCONCLUSIVO, REGRESSAO, PROBLEMA_DE_MEDICAO). GANHO exige o quadro 3 %+ mais curto com
   confiança e nenhuma piora em p95, quadros > 1 s, falhas ou imagem (ver o cabeçalho do script). Leia só o
   veredito; abra o log apenas com PROBLEMA_DE_MEDICAO. Antes de testar algo, veja se já foi decidido
   (`python scripts/motor.py consultar <decisoes.jsonl> <CHAVE>`), e registre o resultado com `--registrar`.
   Um GANHO só vale depois de confirmado numa segunda execução independente.
   O piso de ruído da sua máquina sai de uma execução A/A (os dois braços iguais):
   `python scripts/autopilot.py rodar aa-1 SPARK_AB=VICTORY_STEP_INDEX=0/0` e depois
   `python scripts/motor.py ruido <pasta> --gravar <SPARK_LOG_DIR>/ruido.json`; o veredito usa esse arquivo
   (`--ruido`) para marcar execuções ruidosas demais. (`scripts/veredito.py` e `scripts/ab_estat.py` continuam
   aceitando os mesmos argumentos de antes: chamam o motor.)
6. **Entregar:** se houve ganho real ou a correção funciona, `python scripts/motor.py coordenar entregar <issue>`.
   O jeito depende de quem roda:
   - **Quem pode escrever no repositório** (dono, mantenedores): entrega direta. Rebase na principal, as regras do
     projeto (`verificar.py`), commit e push direto, sem PR e sem esperar outra rodada de testes: o veredito do motor
     e as checagens locais (passo 5) já são a prova. A issue fecha com o commit.
   - **Quem não pode escrever** (a maioria): abre um PR do branch atual com `Fecha #<issue>` e os números
     agregados; quem mantém o projeto revisa. `--pr` força o PR mesmo com permissão, quando você quer revisão.
   Se o avanço for muito relevante (critérios abaixo), o mesmo commit ou PR atualiza o README. Se
   não houve, comentar o resultado na issue, que também é útil, e voltar ao passo 3.

   **Entregas pequenas valem.** Um PR pequeno com um ganho medido (mesmo que modesto, desde que acima do ruído) ou
   com uma correção que funciona já ajuda quem usa o projeto: entregue assim que estiver provado, sem esperar juntar
   um pacote. Um PR por mudança lógica, fácil de revisar; vários ganhos independentes viram vários PRs pequenos.

## Título e builds na mesma máquina: a fila e a carona

Rodar o título e compilar disputam a mesma máquina, e uma fila só (um trabalho por vez) deixa os builds horas
esperando atrás das execuções. O motor traz uma fila com **dois recursos** (`scripts/motor/fila.py`):

- a **placa**: uma execução do título por vez;
- a **CPU**: um build por vez.

Uma execução **exclusiva** (números absolutos, comparação entre execuções; o padrão) segura as duas: nada roda
junto. Uma execução **intercalada** (A/B dentro da execução: `SPARK_AB` ou `SPARK_PLAN`) segura só a placa: um
build pode rodar ao mesmo tempo, porque os braços alternam a cada bloco e sentem a mesma carga; o build então roda
com prioridade baixa e metade dos núcleos. Um build não começa durante uma execução exclusiva, nem quando uma
espera há mais de 20 min; entre builds, o mais curto primeiro (pela mediana do histórico).

```text
python scripts/motor.py fila status
python scripts/motor.py fila rodar build-1 -- cargo build --profile diag -p victory --features diagnostics
python scripts/motor.py fila rodar <nome> --classe jogo --modo intercalada -- <comando que roda o título>
```

`autopilot.py rodar` entra nessa fila sozinho: `intercalada` com `SPARK_AB`/`SPARK_PLAN`, senão `exclusiva`
(`SPARK_FILA_MODO` força um modo). Compile sempre por `motor.py fila rodar`, para a fila saber do build. A pasta da
fila é `SPARK_FILA_DIR`, senão `fila/` dentro de `SPARK_LOG_DIR` (ou da pasta padrão das execuções): fora do
repositório. Todos os agentes e terminais da máquina precisam apontar para a mesma.

No fim da execução fica em `<pasta da execução>/concorrencia.json` o modo e os segundos de build que rodaram junto
(`{"modo", "build_s"}`). O motor lê esse arquivo: uma execução exclusiva com build ao lado é
`PROBLEMA_DE_MEDICAO`; uma intercalada mantém o veredito do seu A/B e só alerta; e `motor.py comparar` recusa
qualquer execução com carga, porque o número absoluto dela mudou.

**Carona.** Vários agentes (ou pessoas) com perguntas de A/B sobre o mesmo executável não precisam de uma execução
cada um. Cada um escreve o seu plano (`SPARK_PLAN`, como em [logs.md](logs.md)) e

```text
python scripts/motor.py plano --juntar <plano-a.toml> <plano-b.toml> ... --saida <SPARK_LOG_DIR>/carona.toml
```

junta todos os experimentos em fatoriais de até 5 fatores: uma execução responde a todos. A saída diz em que
experimento (`--exp N`) e em que fator ficou a pergunta de cada pedido; uma chave pedida duas vezes entra uma vez,
e dois pedidos que mexem na mesma chave de modos diferentes ficam em experimentos separados. Só valem chaves que
alternam dentro da execução (as da tabela `crates/victory-gpu/src/switches.rs`); mudanças que pedem outro
executável continuam pedindo a sua execução.

## Atualizar o README (só avanços muito relevantes)

O AutoPilot atualiza a seção "Onde estamos" e a tabela "Desempenho medido" do README **no mesmo PR** da mudança,
mas só quando o avanço for muito relevante e medido. Isso vale quando:

- a mediana de fps no mundo sobe **10% ou mais** (ou o quadro cai 10% ou mais), na mesma placa e cena, confirmada
  em pelo menos 2 execuções de cada lado;
- os quadros de mais de 1 s caem **pela metade ou mais**, nas mesmas condições;
- um item de "Ainda não" passa a funcionar (por exemplo, programas antes recusados agora rodam, ou um efeito que
  faltava aparece);
- uma placa de outro fabricante, ou de outra geração, ganha a primeira medição publicada.

Ganhos menores, correções internas e melhorias de log não mudam o README; ficam no PR e na issue. Ao atualizar:
números agregados apenas, a data e a placa da medição, nada do jogo (nomes, capturas, logs) e nenhum número
arredondado para cima.

## Lei e limites (inegociáveis)

O AutoPilot existe para melhorar software livre, não para distribuir jogos. Estas regras protegem você e o projeto:

- **Só a sua cópia, obtida legalmente.** O agente nunca baixa, procura, pede ou indica onde conseguir jogos, chaves,
  firmware, SDKs ou arquivos de terceiros.
- **Sem contornar proteção.** O agente não escreve nem usa código para quebrar criptografia, assinatura ou proteção de
  cópia. Se um arquivo estiver protegido, ele para e informa você.
- **Nada do jogo sai da sua máquina.** Nenhum arquivo, trecho de log, captura, quadro, vídeo, dump ou nome de
  conteúdo vai para issues, PRs, commits ou qualquer serviço. Só números agregados (ms, fps, contagens) e código
  próprio ([docs/asset-policy.md](asset-policy.md)).
- **Só código com licença compatível.** Trazer código de outro projeto só se ele for GPL-3.0 ou compatível, com o
  autor e a origem no commit. Nunca copiar código ou documentação proprietária, como SDKs de console.
- **Respeito ao GitHub e às pessoas:** no máximo 3 issues e 3 PRs pequenos por sessão, sempre procurando duplicatas antes, sem
  menções a pessoas que não estão na conversa, sem spam e sem pedir revisão repetidamente.
- **Você responde pelo que o agente publica.** Tudo sai pela sua conta. Revise as primeiras issues e os primeiros PRs.

Este documento não é aconselhamento jurídico. As leis variam por país; em caso de dúvida, não publique.

## Quando o AutoPilot para sozinho

- Não existe a confirmação de `scripts/autopilot.py confirmar`.
- O `game_dir`, os logs ou o cache estão dentro do repositório.
- Um hook ou a CI recusou algo. O agente corrige; nunca usa `--no-verify`.
- Duas execuções seguidas falham na inicialização pelo mesmo motivo: ele abre uma issue com o código de exceção, o
  módulo e o offset, sem log.
- Atingiu o limite de issues/PRs da sessão ou ficou sem um alvo medido.
