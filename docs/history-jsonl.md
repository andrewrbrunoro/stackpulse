# Histórico JSONL do StackPulse

O histórico é salvo automaticamente dentro da **pasta autorizada onde o StackPulse foi iniciado**:

```text
.stackpulse/
  .gitignore        # exclui o conteúdo do Git por padrão
  project.json      # identidade estável e versão do projeto
  sessions/
    <id>.jsonl      # uma conversa por arquivo
    .locks/         # bloqueios locais, sem estado de sessão transportável
```

Copiar a pasta com `.stackpulse` preserva suas conversas mesmo se o caminho mudar. Um clone Git não inclui esse diretório por padrão. Não há busca de um projeto ancestral: iniciar em outra subpasta mantém a autorização e o histórico daquela subpasta. É possível importar explicitamente conversas de outra pasta.

## UI de importação e exportação

Em **Configurações → Conversas salvas**, ou `/sessions`:

- **Importar / I**: cole o caminho de um JSONL. O adaptador detecta StackPulse, Codex ou Claude Code. Aceita até 128 MiB, UTF-8 e uma sessão por arquivo.
- **Exportar / E**: exporta a conversa selecionada como um JSONL autossuficiente para os dados de texto e metadados gravados. O destino deve ser um arquivo novo em uma pasta existente.
- **F5** ou o botão executa a operação local. **Esc** cancela o formulário.

Os atalhos `/import-session` e `/export-session` abrem os mesmos formulários. Fora da lista, a exportação usa a conversa ativa. Nenhum desses comandos inicia um provider.

A importação preserva prompts, respostas públicas de texto, datas e a identificação de origem. Eventos nativos, modelos, ferramentas e telemetria disponíveis permanecem em `source_events` de cada pedido. Não transforma valores ausentes em zero e não soma automaticamente esses dados aos widgets ou aos relatórios de uso. Para métricas Codex, o coletor `sync` continua sendo separado. Respostas de ferramentas não se tornam novos pedidos. Eventos de texto duplicados do Codex são normalizados; a última versão de UUIDs repetidos do Claude é usada.

A mesma origem (`client` + `session_id`) é importada uma única vez por projeto. Reimportar não atualiza nem duplica uma conversa existente, inclusive se o arquivo de origem tiver crescido. Outros clientes e versões incompatíveis são recusados com uma mensagem; os arquivos de origem nunca são modificados.

Importar uma conversa permite **consultar seu histórico e continuar a conversa**. Ao enviar um novo pedido, o histórico textual salvo da sessão é incluído automaticamente no contexto do agente. A importação e a reabertura, por si só, não executam pedidos. Cada envio inicia uma execução do CLI; não retoma a sessão nativa do cliente nem restaura estados internos de ferramentas. Referências a arquivos ou imagens externas não empacotam esses arquivos. Conteúdo de imagem embutido nos eventos de origem é preservado no JSONL, mas não é renderizado no chat importado.

## Formato versão 1

Cada linha é um objeto JSON com:

| Campo | Conteúdo |
|---|---|
| `format` | `stackpulse.session` |
| `version` | `1` |
| `project_id` | Identificador local e portátil do projeto |
| `session_id` | Identificador da conversa |
| `type` | Tipo de evento |

O primeiro evento é sempre `snapshot`, com `document.session.summary`, `document.session.turns`, `document.activities`, `document.messages` e, quando houver, `document.origin`. As exportações materializam o estado atual nesse único registro, terminado por uma quebra de linha.

Durante o uso, os registros seguintes são:

| Tipo | Conteúdo |
|---|---|
| `started` | Pedido, perfil e instante inicial |
| `updated` | `turn_id`, `keep_lines`, `append_lines`, estado, execução e datas |
| `renamed` | Novo título personalizado |
| `activity` | Snapshot de atividade de um pedido, incluindo tempos dos agentes |
| `agent_message` | Orientação/intervenção e situação da entrega |
| `finished` | Fechamento da conversa |
| `resumed` | Reabertura visual; pedidos inacabados ficam interrompidos |

`updated.keep_lines` conserva as primeiras N linhas da resposta anterior; `append_lines` substitui o restante. Isso evita gravar toda a resposta a cada atualização. Atualizações idênticas não geram registros. Não se trata de um registro de cada evento interno do provider: são os eventos e snapshots observados pelo StackPulse.

O campo `execution` preserva o snapshot disponível do pedido: configuração da equipe, modelo, consumo, tempo e feedback. Os relatórios gerais e o coletor de consumo continuam usando o SQLite principal; esta mudança não torna todos os dados desse banco reconstruíveis apenas pelo histórico de conversa. Exportações nativas do StackPulse preservam esses snapshots e a UI recupera registros de execução ausentes ao importar ou abrir o projeto em um banco novo.

## Migração e recuperação

Na abertura, o StackPulse consulta o antigo `<banco-de-métricas>.chat.sqlite` em modo de leitura e migra as conversas vinculadas à pasta atual. Mantém seus IDs, títulos, datas, pedidos, respostas, execuções, últimas atividades e intervenções. O arquivo antigo permanece intacto. Sessões ainda em uso pela versão anterior são adiadas; feche o terminal antigo e abra novamente. Depois de mover uma pasta ainda não migrada, migre no local antigo ou importe uma exportação existente.

Um bloqueio do sistema operacional protege cada conversa contra dois escritores simultâneos. Arquivos de sessão e exportações são criados atomicamente. Cada evento completo termina em `\n`; uma última linha sem terminador é tratada como gravação interrompida. Ao reabrir para escrita, o original é copiado para `.jsonl.recovery-<id>` antes de remover a cauda incompleta. Erros em linhas completas e versões futuras não são descartados silenciosamente; a conversa fica indisponível e a lista mostra o problema. Os demais arquivos continuam acessíveis.
