# ADR 0001 — Comercialização do fork do Handy

| Versão | Atualizado em |
| ------ | ------------- |
| 1      | 13/10/2026    |

## Status

Aceito.

## Contexto

Este repositório é um fork de [cjpais/Handy](https://github.com/cjpais/Handy) (remote `upstream`),
mantido em `lucaslagrimante-planejar/Handy` (remote `origin`). O Handy é um app desktop de
speech-to-text offline (Tauri 2: Rust + React) que **já roda em Windows, macOS e Linux**.

Sobre ele estamos construindo funcionalidades próprias, a principal sendo o **modo reunião**
(transcrição de microfone + áudio do sistema, separação de falantes, resumo e ata). A intenção é
vender o produto (licença ou assinatura).

Levantamento de licenças feito em 13/10/2026:

- Código do Handy: **MIT** (`LICENSE`, "Copyright (c) 2025 CJ Pais").
- Dependências Rust e frontend relevantes: todas MIT e/ou Apache-2.0 (tauri, rdev, enigo,
  transcribe-rs, transcribe-cpp, rodio, rusqlite, whatlang, ferrous-opencc, React, i18next,
  zustand…). Nenhuma GPL/AGPL.
- Modelos de IA têm licenças próprias, diferentes do código — ver
  [`features/geral/licenciamento-e-modelos.md`](../features/geral/licenciamento-e-modelos.md).

## Decisão

1. **Vender o fork como produto próprio é permitido** e é o caminho adotado. A MIT permite uso
   comercial, venda, assinatura, modificação e código novo fechado.
2. **Continuar recebendo atualizações do upstream** (merge de `upstream/main`). Tudo o que já foi
   publicado sob MIT permanece MIT; a cada sincronização conferir se o `LICENSE` mudou — ver
   [`runbooks/sincronizar-com-upstream.md`](../runbooks/sincronizar-com-upstream.md).
3. **Atribuição obrigatória, sem destaque no marketing.** O texto da licença MIT e o copyright do
   CJ Pais acompanham o produto (tela "Sobre / Licenças de terceiros"). Não é necessário anunciar
   na comunicação comercial que a base é gratuita/open source.
   - Recomendado (opcional): uma linha discreta "Baseado no projeto open source Handy" no "Sobre",
     para evitar repercussão negativa caso usuários técnicos identifiquem a origem.
4. **Nada de afirmação falsa** (CDC art. 37 — publicidade enganosa): não dizer "tecnologia
   proprietária", "desenvolvido do zero", nem vender como exclusivo o que o Handy já faz (ex.:
   ditado por atalho). Omitir a origem é aceitável; afirmar algo falso não.
5. **Nome e identidade visual próprios.** A licença do código não cobre a marca "Handy", o logo
   nem o domínio `handy.computer`. O produto não pode sugerir ser oficial/endossado pelo upstream.

## Proposta de valor (por que alguém paga, se existe a versão gratuita)

O upstream é gratuito e público, então o valor precisa estar no que acrescentamos:

- **Modo reunião em Windows e macOS** (hoje só Linux — ver [ADR 0003](0003-modo-reuniao-multiplataforma.md)).
  É o maior bloqueio comercial.
- **Ata pronta para usar**: modelos por tipo de reunião, exportação .docx/PDF, participantes
  nomeados.
- **Histórico pesquisável** de reuniões.
- **Integrações**: agenda (início automático), Drive, Notion, envio da ata por e-mail.
- **Setup sem fricção**: detecção de hardware e download automático do modelo adequado — é o que
  separa o usuário leigo do upstream.

## Modelo de cobrança

Sem custo de infraestrutura de IA (ver [ADR 0002](0002-processamento-local-e-provedor-escolhido-pelo-usuario.md)),
os modelos coerentes são:

- licença única com 1 ano de atualizações (modelo Sublime/MacWhisper); ou
- assinatura de valor baixo.

Assinatura cara sem serviço em nuvem é difícil de justificar.

## Consequências / pendências antes do lançamento comercial

- [ ] **Updater** (`src-tauri/tauri.conf.json`, `plugins.updater`): ainda aponta para
      `github.com/cjpais/Handy/releases` com a pubkey do upstream. Sem trocar endpoint e gerar chave
      própria, o build oficial do Handy seria instalado por cima do nosso. **Crítico.**
- [ ] Rebranding (nome, ícones, identifier `com.pais.handy`, textos).
- [ ] Tela "Sobre": licença MIT do Handy + atribuições dos modelos (hoje só cita ggml, em
      `src/components/settings/about/AboutSettings.tsx`).
- [ ] Termos de uso e política de privacidade (ver ADR 0002).
- [ ] Revisão por advogado de propriedade intelectual (este ADR é análise técnica, não parecer
      jurídico) e registro da marca própria.
