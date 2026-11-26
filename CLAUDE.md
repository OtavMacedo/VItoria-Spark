# CLAUDE.md: instruções para assistentes de IA no VItoria Spark

Este arquivo vale para qualquer assistente de código (Claude Code e similares) que trabalhe neste repositório.
As regras de [CONTRIBUTING.md](CONTRIBUTING.md) e de [docs/asset-policy.md](docs/asset-policy.md) valem por inteiro.
Abaixo está o que um assistente mais erra, em forma de ordem.

## O projeto

Camada de compatibilidade Xbox Series → Windows x64, em Rust e Direct3D 12, fork brasileiro do VIctory. O código x86-64
do título roda no processo; nós fornecemos carregador (`victory-loader`, `victory-pe`), serviços do sistema
(`victory-hle`), GPU (`victory-gpu`), tradutor de shaders GFX10 → DXIL (`victory-shader`), formatos
(`victory-formats`) e o executável (`apps/victory`). Foco: **FPS e imagem correta**.

## Regras absolutas

1. **Nunca** coloque no repositório nada de jogo nem nenhuma saída de execução: logs, `resumo.md`, `eventos.jsonl`,
   capturas, dumps e crash reports. Não cole trechos de log em comentários, testes ou commits. Números agregados
   (ms, fps, contagens) podem ir só na descrição do PR.
2. **Nunca** escreva nomes de títulos, personagens, lugares ou missões em código, comentários, testes, docs ou
   mensagens. Use "cena pesada", "primeira área", "N desenhos por quadro".
3. **Nunca** inclua caminhos da máquina, nomes de usuário ou e-mails. Use `<HOME>` e `<JOGO>` em exemplos.
4. **Nunca** adicione `Co-authored-by` nem menção à ferramenta em commits e PRs. O hook `commit-msg` recusa.
5. **Nunca** desative ou contorne os hooks (`--no-verify`) nem a política (`cargo xtask policy`).
6. Não rode o título sem que a pessoa peça. Testes automáticos usam dados sintéticos, WARP ou a GPU, nunca arquivos
   de jogo.

## Comentários no código (rígido)

- Em **inglês**, curtos, explicando o **porquê**: restrição, invariante ou comportamento do console imitado.
- Doc comment (`///`) em todo item `pub`/`pub(crate)` não trivial, com unidades e o comportamento em erro.
- `// SAFETY:` em todo `unsafe`, cobrindo tempo de vida, tamanho e fim do trabalho da GPU.
- Proibido: código comentado, `TODO` sem issue (`TODO(#123)`), comentário que narra o código linha a linha,
  histórico ("was", "fixed", "now"), medições presas a uma cena, nomes de pessoas e de títulos, e emojis.
- Não reescreva comentários de código alheio que não estiver mudando. Ao mudar uma função, atualize os comentários
  dela.

## Como trabalhar

- Antes de mudar algo, leia o código ao redor e os testes do módulo. Siga o estilo existente: nomes que dizem unidade
  e escopo, erros com contexto e nenhum `unwrap` em caminho do título.
- Toda opção nova começa desligada, com uma variável `VICTORY_*`/`SPARK_*` documentada no doc comment.
- Desempenho: meça antes e depois. Use o `resumo.md` da sessão (ver [docs/logs.md](docs/logs.md)) e compare mediana e
  p95 do quadro e os componentes. Uma rodada só é ruído: faça pelo menos duas de cada lado, com caches aquecidos.
- Imagem: na primeira rodada depois de mudar o tradutor, as pipelines são compiladas durante o jogo e desenhos são
  descartados. Não julgue a imagem nessa rodada.
- Antes de concluir: `cargo fmt`, `cargo clippy ... -D warnings`, testes do crate tocado e `cargo xtask policy`.
- Para analisar uma execução, comece pelo `resumo.md` (seção "Próximo alvo") e só depois filtre o `eventos.jsonl` pelo
  `target` que interessa. Não carregue o log inteiro no contexto.

## AutoPilot Mode

Quando a pessoa pedir "AutoPilot Mode" (ou apontar este repositório e a pasta do título pedindo para contribuir
sozinho), siga **[docs/autopilot.md](docs/autopilot.md)** à risca, em ciclo: sincronizar, medir, escolher o alvo,
abrir ou comentar a issue, implementar, provar com A/B e entregar o PR. Pontos em que um agente costuma errar:

- **Antes de tudo**, confira se existe a confirmação de `python scripts/autopilot.py confirmar`. Se não existir, peça
  que a pessoa a rode (`! python scripts/autopilot.py confirmar`). **Nunca** rode `confirmar` nem digite a declaração
  no lugar dela.
- Rode o título só por `python scripts/autopilot.py rodar <nome>`: janela escondida, sem som, para quando a cena estável
  está medida (ou com `SPARK_AB`, quando o A/B decide), logs fora do repositório e uma execução por vez. Não feche
  processos que não iniciou.
- Mudou o tradutor ou uma opção dele? Rode `python scripts/autopilot.py aquecer [CHAVE=valor ...]` antes de medir:
  é bem mais rápido traduzir antes, em todos os núcleos, do que durante o jogo.
- Leia o `resumo.md` primeiro. Em issues e PRs vão só números agregados. Nunca cole log, nome de conteúdo, caminho ou
  captura.
- Antes de abrir uma issue ou um PR, procure duplicatas (`gh issue list --label autopilot`, `gh pr list`). No máximo
  3 issues e 1 PR por sessão. Use os modelos em `.github/`.
- Nunca baixe, procure ou indique arquivos de jogo, chaves, firmware ou SDKs. Nunca escreva código que quebre
  criptografia ou proteção de cópia. Se encontrar algo assim no caminho, pare e avise a pessoa.
- Sem ganho medido, não há PR: registre o resultado na issue e escolha outro alvo.
- README ("Onde estamos" e "Desempenho medido") só muda com avanço **muito relevante**: +10% de fps ou −50% de
  quadros > 1 s confirmados em 2+ execuções de cada lado, um item de "Ainda não" resolvido, ou a primeira medição de
  uma placa nova ([docs/autopilot.md](docs/autopilot.md)). No mesmo PR da mudança; nunca arredonde para cima.

## Onde ficam as coisas

- Build: `cargo build --release -p victory`; diagnóstico: `cargo build --profile diag -p victory --features diagnostics`.
- Configuração: `victory.toml` (modelo em `victory.example.toml`); `victory.exe config` mostra a configuração resolvida.
- Logs de sessão: `SPARK_LOG_DIR`, sempre fora do repositório.
