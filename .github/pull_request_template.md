## O que muda e por quê

Fecha #

## Prova
- [ ] Testes de unidade (e WARP/GPU para tradutor ou GPU) que falham antes e passam depois
- [ ] A/B no mesmo build, caches aquecidos, pelo menos 2 execuções de cada lado

| | Antes | Depois |
|---|---|---|
| Quadro (mediana / p95) | | |
| Componentes (preparo / GPU / cópia de volta) | | |

GPU e driver · resolução · CPU · RAM

Testado em: - [ ] NVIDIA - [ ] AMD - [ ] Intel. Se mudar código comum ou `perfis/comum.toml`, diga o risco para as
placas em que não foi testado.

## Regras
- [ ] Nada de jogo nem saída de execução (docs/asset-policy.md)
- [ ] Comentários conforme CONTRIBUTING.md (regra 5)
- [ ] Opção nova desligada por padrão, ou medição que justifique ligar
