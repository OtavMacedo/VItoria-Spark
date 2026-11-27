# Política de arquivos (regra nº 1)

**Nada de nenhum jogo entra neste repositório, nunca.** Tudo é gerado na máquina de quem usa, a partir de arquivos
que a própria pessoa fornece, e fica fora do repositório.

## Proibido, em qualquer forma

Arquivos, literais no código, fixtures, snapshots, documentação, issues, PRs e comentários:

- conteúdo de jogo: modelos, texturas, shaders, áudio, vídeo, texto, arquivos compactados;
- o executável ou qualquer parte dele: seções de dados, strings, tabelas, recursos, código além das assinaturas
  permitidas abaixo;
- qualquer coisa derivada disso: assets convertidos, código descompilado ou desmontado, dumps de shaders, tabelas
  extraídas;
- **qualquer saída de uma execução**: logs, `resumo.md`, `eventos.jsonl`, capturas de tela, quadros, vídeos, dumps de
  memória e relatórios de crash. Nem resumidos, nem anonimizados;
- nomes de títulos, personagens, lugares ou missões, também em comentários e mensagens de commit.

## Permitido

- nosso próprio código que entende formatos;
- identidades de build: BLAKE3 de um executável, GUID/age do CodeView;
- offsets por build de funções que interceptamos, com nomes escolhidos por nós;
- assinaturas de bytes que localizam funções ou dados;
- pequenas constantes de interoperabilidade (números mágicos, valores de enum) escritas por nós;
- dados de teste sintéticos gerados pelo nosso código;
- números agregados de desempenho em PRs (ms por quadro, fps, contagens), sem nome de cena nem imagem.

## A única exceção

`crates/victory-formats/src/bcpack-tables.bin` (cerca de 78 KB): as tabelas constantes do formato BCPack. Sem elas
nenhum fluxo BCPack é decodificado, e elas não podem ser derivadas. Herdada do VIctory e registrada em
`xtask/src/policy.rs`. Nenhuma outra exceção é aceita.

## Onde ficam os dados

Fora do repositório, configurados em `victory.toml`: `game_dir` (os seus arquivos), `cache_dir` (tudo o que é
derivado) e a pasta de logs (`SPARK_LOG_DIR`). As ferramentas se recusam a escrever saída derivada abaixo de uma
pasta que contenha `.victory-root`.

Com `SPARK_OFFLINE=1`, a pasta da sessão ganha `offline/`: os shaders do título que falharam, para serem
verificados sem ele ([motor.md](motor.md)). São dados do jogo como quaisquer outros: ficam lá e nunca são publicados.

## Como é imposta

1. `.gitignore` mantém os tipos comuns de dados fora do `git status`.
2. O hook `pre-commit` roda `cargo xtask policy --staged` (tipos de arquivo, tamanho, assinaturas de formatos
   binários) e `scripts/verificar.py --staged` (nomes proibidos, dados pessoais, regras de comentários).
3. O hook `commit-msg` verifica a mensagem; a CI repete tudo em cada PR.
4. A revisão humana é a última barreira: um PR que viole esta política é fechado sem discussão.

Ative os hooks em cada clone: `git config core.hooksPath .githooks`.
