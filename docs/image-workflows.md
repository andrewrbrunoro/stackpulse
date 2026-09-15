# Da imagem à execução acompanhada

O Timeline tem dois modelos conceitualmente distintos: o **auxiliar** interpreta a imagem e gera um perfil; o **orquestrador** e seus subagentes executam pedidos usando esse perfil. O auxiliar não precisa ser o modelo da raiz da equipe.

## Setup

```sh
cargo run --release -- setup
```

O assistente abre uma interface visual no terminal em quatro etapas: **CLI, Auxiliar, Projeto e Resumo**. Na primeira, ↑/↓ selecionam o CLI e `Enter` continua. Nos campos, `Enter` avança, `Tab`/`Shift+Tab` ou ↑/↓ mudam o foco, ←/→ movem o cursor e `Ctrl+U` limpa o valor. É possível colar texto e editar caminhos com espaços e acentos. O esforço continua sendo informado pelo identificador aceito pelo CLI.

`Esc` volta uma etapa e mantém o rascunho; na primeira etapa, sai. `Ctrl+C` cancela a qualquer momento. O setup só grava ao pressionar `Enter` no resumo. Valores inválidos aparecem na própria tela. O terminal é restaurado ao sair.

A interface precisa de pelo menos 56 colunas e 24 linhas. Se a janela ficar menor, o rascunho é mantido enquanto você redimensiona. Use `setup --plain` para o fluxo de perguntas em texto ou `NO_COLOR=1` para desativar cores. Com `TERM=dumb`, o setup usa perguntas em texto automaticamente. Flags de configuração continuam funcionando sem interação.

O assistente começa mostrando os CLIs instalados que reconhece, com nome, caminho e status. A ordem do catálogo é: Codex, Claude Code, Gemini CLI, GitHub Copilot, Cursor, Grok, OpenCode, Aider, Amp, Droid, Goose e Kiro. Procura arquivos executáveis no PATH e em pastas comuns de instalação local e do sistema. O inventário cobre esse catálogo, não qualquer software arbitrário instalado na máquina.

A descoberta usa o filesystem: não inicia os CLIs, não autentica e não chama modelos. Um executável encontrado pode ainda precisar de login. O nome `agent` é compartilhado por Cursor e Grok; a descoberta usa o caminho e o destino do symlink para identificar o provider quando possível, e sinaliza aliases ambíguos.

**Codex CLI, Claude Code, Cursor Agent e Grok CLI** têm adaptadores de execução e podem ser selecionados. Os demais aparecem como **SEM ADAPTADOR**: foram detectados, mas o Timeline ainda não implementa seus protocolos. Isso não indica problema na instalação ou na assinatura. O login existente do client escolhido é reutilizado, sem pedir uma chave de API adicional.

Depois da seleção, o setup pede provider, modelo com visão, esforço, pasta `providers`, perfil padrão e limite de subagentes simultâneos. Confirmar outro client aplica seus padrões; retornar ao anterior recupera os campos editados. Apenas navegar pela lista não altera os valores.

Para consultar apenas o inventário, sem criar ou atualizar configuração, banco ou pastas:

```sh
cargo run --release -- setup --list-clis
cargo run --release -- setup --list-clis --json
```

`--list-clis` não pode ser combinado com flags que alteram a configuração do setup. Uma configuração pode ser criada também sem interação; nesse caso, o inventário continua sendo mostrado:

```sh
cargo run --release -- setup \
  --client codex --provider openai --model gpt-6-astra --effort medium \
  --providers-root providers --default-profile cost_efficient_gpt --max-agents 4

# Reutilizar modelo, esforço e login configurados no client
cargo run --release -- setup --client claude
cargo run --release -- setup --client cursor
cargo run --release -- setup --client grok

# Um nome conhecido identifica o client; wrappers precisam de --client
cargo run --release -- setup --executable claude
cargo run --release -- setup --client claude --executable /caminho/meu-wrapper
```

O campo `client` identifica o programa; `provider` identifica o serviço da equipe. Os padrões são:

| Client | Provider | Modelo | Esforço |
|---|---|---|---|
| `codex` | `openai` | `gpt-6-astra` | `medium` |
| `claude` | `anthropic` | `default` | `default` |
| `cursor` | `cursor` | `default` | `default` |
| `grok` | `xai` | `default` | `default` |

`default` deixa o CLI usar a própria configuração de modelo ou esforço. No Codex, `provider` também pode ser um ID já configurado nesse CLI. A disponibilidade do modelo e do esforço explícitos é validada pelo client/provider ao chamá-lo. Um wrapper deve implementar a interface do backend informado; o caminho personalizado salvo é preservado ao repetir o setup.

O setup fica em `~/.config/ai-token-timeline/config.json`. `--config caminho.json` seleciona outro setup; `--db` continua selecionando o banco. A pasta `providers_root` fica absoluta, permitindo usar o programa ao entrar em outros diretórios.

## Imagem e perfil

```text
providers/
  project/
    all/
      cost_efficient_gpt.png
      cost_efficient_gpt.md
    meu-projeto/
      agressive_team.png
      agressive_team.md
```

O exemplo OpenAI incluído usa a imagem fornecida nesta conversa. Outros perfis podem vir de imagens compartilhadas pelo usuário:

```sh
ai-token-timeline profiles add /caminho/agressive_team.png --scope all
ai-token-timeline profiles add /caminho/agressive_team.png --scope meu-projeto
ai-token-timeline profiles list
```

`profiles add` copia a imagem e chama o auxiliar para interpretá-la; `--no-compile` apenas copia. Também é possível colocar uma imagem diretamente na pasta e executar:

```sh
ai-token-timeline profiles compile providers/project/all/agressive_team.png
```

O auxiliar recebe a imagem como dado, com um schema JSON que descreve a equipe. O programa valida a resposta e escreve um Markdown com frontmatter TOML, tabela de papéis, condições de delegação e integração. Nenhum comando presente na imagem é executado durante a extração. O modelo é orientado a interpretar somente a configuração da equipe; a compilação usa as restrições de leitura do adaptador e não permite delegação.

Codex, Claude e Grok recebem a imagem como entrada multimodal. Cursor usa a ferramenta de leitura de imagem do próprio CLI, em modo `ask` e com sandbox. O modelo escolhido precisa ter visão; `default` preserva a escolha local, mas não garante essa capacidade em qualquer modelo. Claude aceita imagens de até 5 MiB no adaptador.

Campos não identificáveis ficam como `unknown`, com uma explicação. O Markdown pode ser salvo como rascunho, mas `run` exige resolver esses campos no TOML antes de executar. Modelos, esforços e provedor não são substituídos silenciosamente. O hash da imagem detecta quando ela muda; recompile com `--force` para atualizar um perfil existente.

O bloco TOML é a fonte executável. Editar apenas a tabela descritiva abaixo dele não muda a configuração. O hash do Markdown completo identifica sua revisão. Tentativas de extração ficam em `nome.compile.jsonl`, com provider/modelo/esforço, tokens reportados e status, sem o texto gerado. Esse consumo é separado das notas de entrega das tarefas.

## Entrar no projeto e enviar o pedido

```sh
cd /caminho/meu-projeto
ai-token-timeline run "Implemente a busca por nome e verifique o resultado"
```

Dentro de um repositório, a chave do projeto é o nome da raiz Git, inclusive quando o comando parte de uma subpasta. Fora de Git, usa o nome da pasta atual. Projetos com o mesmo nome compartilham a seleção de perfil; os registros e gráficos continuam separados pelo caminho absoluto.

Uma entrada em `project/meu-projeto` substitui a entrada de mesmo nome em `project/all`. Uma imagem local ainda não compilada também tem prioridade sobre o Markdown global. Se houver apenas um perfil, ele é selecionado automaticamente; com vários, escolha um no setup ou use `--profile`. Uma imagem pendente é compilada antes da execução normal.

```sh
ai-token-timeline run "Seu pedido" --profile agressive_team
ai-token-timeline run "Seu pedido" --dry-run
ai-token-timeline run "Analise este módulo" --sandbox read-only
```

`--dry-run` mostra o perfil, o CLI executor e o pedido preparado, sem chamar a LLM, sem compilar imagens e sem registrar uma execução. O auxiliar e o executor podem ser diferentes: um auxiliar Claude pode interpretar uma imagem com uma equipe Astra/Luna, e `run` usará Codex para o provider `openai` dessa equipe. Se o provider da equipe coincide com o do setup, o executável configurado é preservado; caso contrário, o Timeline procura o client instalado correspondente a `openai`/Codex, `anthropic`/Claude, `cursor`/Cursor ou `xai`/Grok.

Por padrão, a execução permite escrita no workspace e respeita as permissões nativas do CLI, sem flags `force` ou `yolo`. Ferramentas que exigem uma autorização não disponível no processo podem ser bloqueadas pelo client. Em `read-only`, perfis sem subagentes usam apenas ferramentas de leitura no Claude, uma lista explícita de ferramentas de leitura no Grok e modo `ask` com sandbox no Cursor. Equipes Claude com subagentes exigem `workspace-write`: em `read-only`, a execução é recusada sem ampliar permissões.

A equipe do perfil é aplicada automaticamente ao enviar o pedido. O usuário não precisa pedir subagentes nem explicar a distribuição do trabalho. No chat, o cabeçalho mostra a equipe configurada e `/team` apresenta o orquestrador, todos os papéis, modelos/esforços, finalidades, condições e integração; essa consulta é local e não inicia agentes.

Em `on_demand`, o orquestrador delega apenas quando a tarefa justificar os papéis disponíveis, sem criar todos por obrigação. Em `parallel`, pode distribuir partes independentes dentro do limite configurado; em `sequential`, usa no máximo um subagente por vez, aguardando o anterior. As condições e a integração do perfil acompanham as instruções da execução.

| Adaptador | Equipe configurada no perfil |
| --- | --- |
| Codex | Aplica modelo, esforço e instruções por papel em arquivos temporários e opções nativas da invocação; a concorrência tem limite nativo |
| Claude | Aplica modelo, esforço e instruções por papel via `--agents`, somente em `workspace-write`; a concorrência é solicitada pelo prompt, sem imposição nativa pelo adaptador |
| Cursor | Perfis com subagentes são recusados antes de iniciar o CLI porque o adaptador não garante modelo/esforço por papel |
| Grok | Perfis com subagentes são recusados antes de iniciar o CLI porque a configuração local pode substituir os modelos/esforços solicitados |

As configurações nativas usadas para os papéis valem para aquela invocação e não alteram os arquivos salvos dos CLIs. Os arquivos temporários do Codex são removidos ao encerrar. Cursor e Grok continuam disponíveis para perfis sem subagentes; essa restrição não indica problema na instalação ou assinatura. Um perfil com vinte papéis não comprova que vinte agentes foram usados: `/agents` e as métricas dependem da telemetria observada.

Antes de iniciar, o Timeline grava um registro com UUID, cópia integral do Markdown, stack planejada, hash/revisão, configuração do auxiliar, limite de concorrência, sandbox, caminho do projeto e hash/tamanho do pedido. O prompt completo não é guardado no banco do Timeline; o CLI usado pode manter seu histórico normal.

O registro recebe depois duração de ponta a ponta, status, `execution_client`, `cli_session_id`, `observed_model`, contadores e cobertura reportados pelo CLI. Quando disponível, `reported_cost_usd` guarda a estimativa de custo fornecida pelo client; esse valor não representa uma fatura nem cobrança adicional da assinatura. Para Codex, o registro também recebe métricas da árvore encontradas nos logs locais. Falhas do processo, timeout e cancelamento por `Ctrl+C` ficam registrados. `--timeout` configura o limite da invocação em segundos. Um encerramento forçado do Timeline, como `kill -9` ou queda do sistema, pode deixar um registro `running`; esse estado não entra como execução terminada na tendência.

```sh
ai-token-timeline executions
ai-token-timeline executions --json
```

A cobertura é explícita:

| Cobertura | Significado |
|---|---|
| `local_observed` | Métricas da árvore encontrada nos logs locais, com turnos fechados e eventos vinculados a tempo. Não comprova que logs de todos os subagentes estejam disponíveis. |
| `local_partial` | Telemetria local com raiz, tempo ou encerramentos faltantes. |
| `cli_tree` | O CLI reportou consumo agregado da invocação, incluindo a árvore que ele contabiliza. Não informa necessariamente quantos agentes ou turnos participaram. |
| `cli_partial` | O CLI forneceu apenas parte do consumo; não é um total completo da execução. |
| `root_only` | Apenas os tokens reportados pelo processo da raiz; subagentes não são presumidos. |
| `unavailable` | O provider/CLI não forneceu contadores utilizáveis. |

No Codex, os contadores de stdout não são somados novamente aos logs locais. `executions` e `trend` atualizam a associação com os dados já importados; use `sync` para coletar logs que chegaram depois. O fallback Codex `root_only` aparece no registro e nos dados da tendência, sem ser inserido de novo no coletor global.

Claude, Cursor e Grok são inseridos no histórico normalizado como **uma sessão observada por invocação**, alimentando o widget de consumo. Contadores intermediários e totais finais não são somados duas vezes. Os tokens são atribuídos ao horário de encerramento; o intervalo medido é o tempo de ponta a ponta. Um total `cli_tree` pode reunir vários modelos, por isso não é atribuído integralmente ao modelo da raiz para calcular custo. O agregado não revela o número de agentes nem permite reconstruir o tempo individual deles. Se o CLI não expõe contadores, os tokens permanecem indisponíveis.

## Feedback e linha suavizada

Depois da execução, em terminal interativo, o programa pergunta:

1. **Foi entregue?** Sim (`1`), parcialmente (`0.5`), não (`0`) ou pular (`p`).
2. **Foi rápido?** De `1` (muito lento) a `5` (muito rápido).
3. Uma observação opcional.

Pular não equivale a nota zero. Para registrar ou corrigir depois:

```sh
ai-token-timeline feedback ID_DA_EXECUCAO --delivered 0.5 --speed 2 --note "Faltou a paginação"
ai-token-timeline trend --profile agressive_team --days 30
ai-token-timeline trend --profile agressive_team --days 30 --json
ai-token-timeline widget --quality
```

No widget, `g` alterna consumo e feedback. A visão compacta mostra o grupo mais recente; `trend` lista todos os grupos. A escala de entrega vai de 0 a 100%. A rapidez percebida é normalizada de 1–5 para 0–100% e tem sua própria linha, sem ser misturada à entrega.

```text
Q_d = média das avaliações de entrega no dia d
S_d = α × Q_d + (1 − α) × S_anterior
```

A softline usa EWMA com `α=0.3`; `trend --alpha` permite ajustar. Dias sem avaliação aparecem como lacunas e não atualizam o valor suavizado. As curvas ficam separadas por client executor, revisão do perfil, projeto, benchmark, stack observada, cobertura, sandbox e limite de concorrência. O custo diário usa as tarifas cadastradas quando a cobertura de preço é completa; caso contrário, pode usar a estimativa reportada pelo CLI. Valores desconhecidos não viram zero.

Isso mostra mudanças na experiência do usuário, mas não determina sozinho se um provedor reduziu capacidade. Pedidos de dificuldades diferentes, mudanças de contexto, latência, ferramentas e expectativas também afetam as notas. Feedback cotidiano aparece como tal, sem atribuição ao provedor.

Para comparações controladas, use o mesmo identificador de benchmark/versionamento:

```sh
ai-token-timeline run "Pedido de benchmark fixo" --benchmark suite-v1
ai-token-timeline tag ID_DA_SESSAO_RAIZ --baseline
ai-token-timeline benchmark-compare
```

O feedback de entrega também preenche a anotação de qualidade da sessão raiz quando há benchmark. O identificador recebe um sufixo de comparação que distingue revisão, projeto, concorrência, sandbox e hash do pedido. `tag --baseline` preserva esse identificador e a nota existentes. Nas tendências com benchmark, prompts diferentes também ficam separados. Com pelo menos dez feedbacks distribuídos em três dias, uma queda de 10 pontos percentuais nas cinco avaliações mais recentes frente às anteriores aparece como sinal exploratório para investigar. `compare` mantém a análise de benchmark com intervalo bootstrap descrita no README. Nenhum desses sinais comprova alteração interna do modelo.

Referências: [Codex em modo não interativo](https://learn.chatgpt.com/docs/non-interactive-mode) e [modelos e controles de subagentes](https://learn.chatgpt.com/docs/agent-configuration/subagents). Os adaptadores também seguem os protocolos de saída dos CLIs instalados. A validação automatizada cobre comandos, extração e normalização de eventos; não equivale a um benchmark real de vinte subagentes em cada provider.
