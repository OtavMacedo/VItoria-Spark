# Regras de contribuição

As regras são rígidas e valem para todo commit, PR, issue e comentário. O hook e a CI verificam as que podem ser
verificadas. As outras são verificadas na revisão. Um PR que quebre qualquer uma é fechado.

Antes do primeiro commit, em cada clone: `git config core.hooksPath .githooks`.

## 1. Nenhum arquivo de jogo

Siga [docs/asset-policy.md](docs/asset-policy.md) à risca. Nada de jogo, nada derivado de jogo e nenhuma saída de
execução: logs, capturas, quadros, vídeos e dumps. Também não cite nomes de títulos, personagens, lugares ou missões,
nem em comentários nem em mensagens de commit. Descreva cenas de forma neutra: "cena pesada, ~4.000 desenhos por
quadro".

## 2. Nenhum dado pessoal

Nada de caminhos absolutos da sua máquina (pasta de usuário, pastas de download), nomes de usuário, e-mails pessoais, IPs ou
números de série. Use identidade git com e-mail `noreply`. Os logs do Spark já trocam esses dados por `<HOME>` e
`<JOGO>`. Mesmo assim, eles nunca entram no repositório (regra 1).

## 3. Toda mudança é medida ou testada

- **Correção:** teste de unidade que falha antes e passa depois. Para GPU e tradutor, um teste em hardware ou no WARP
  que relê o resultado esperado.
- **Desempenho:** antes e depois no mesmo build, com o mesmo save e caches aquecidos (shaders e pipelines). Pelo menos
  duas rodadas de cada lado. Informe a mediana e o p95 do tempo de quadro, os componentes (preparo, GPU, cópias), a GPU,
  o driver e a resolução. Um fps sozinho não basta.
- **Imagem:** a otimização mantém a imagem e as dependências de escrita. Descreva o que foi conferido, sem anexar
  capturas (regra 1).
- Toda opção nova começa desligada (`VICTORY_*`/`SPARK_*`) até ter medição que justifique ligá-la por padrão.

## 4. Commits e PRs

- Um commit por mudança lógica. Primeira linha no imperativo, em inglês, com até 100 caracteres, e corpo explicando
  o porquê.
- Mudança trazida de outro projeto: o autor original fica como autor do commit, e a origem (repositório, PR ou commit)
  vai no corpo.
- Sem trailers `Co-authored-by` e sem menção a ferramentas que geraram o código. Quem assina o commit responde por ele.
- Branch atualizada com a `main` por rebase, sem merge commits.
- Antes do PR, tudo isto passa:

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked --exclude victory-gpu
cargo test -p victory-gpu --locked -- --test-threads=1
cargo xtask policy
```

## 5. Código e comentários

- **Comentários de código em inglês.** Docs, README e textos de PR podem ser em pt-BR.
- Um comentário explica o porquê: uma restrição, um invariante ou o comportamento do console que o código imita. O
  que o código faz se lê no código.
- Todo item `pub` e `pub(crate)` não trivial tem doc comment (`///`) com unidade, faixa e o que acontece no erro.
- Todo bloco `unsafe` tem um comentário `// SAFETY:` dizendo por que é seguro: tempo de vida, tamanho, alinhamento e
  fim do trabalho da GPU antes de reciclar.
- Unidade e escopo ficam no nome ou no comentário: `_bytes`, `_ms`, `_pages`, endereço do título ou do host, RVA ou
  offset no arquivo.
- Proibido: código comentado, `TODO`/`FIXME` sem issue (use `TODO(#123)`), comentários que repetem o código,
  comentários de histórico ("antes era assim", "corrigido em…"; isso vai no commit), números de medição presos a uma
  cena, menção a pessoas, nomes de títulos e dados pessoais.
- Comentários curtos: uma linha quando possível, no máximo um parágrafo antes de uma função.
- Nada de `unwrap()` ou `expect()` em caminho de execução do título sem um comentário que prove que não falha. Erros
  sobem com contexto.

## 6. Escopo

O foco do projeto é **desempenho e imagem correta**. Mudanças que dependem do nome de um título ou de um endereço de um
título específico não entram no motor, com exceção dos hooks por build descritos na política de arquivos.
