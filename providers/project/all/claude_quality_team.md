+++
schema_version = 1
source_image = "astra_quality_team.png"
source_sha256 = "436eed15a6c938e5b6419f23860dbd1782fcb15b754d950c5c2497274b1c75ef"
generated_by = "stackpulse:user-request"
generated_at = "2026-09-15T19:29:11.433380Z"

[team]
name = "claude_quality_team"
provider = "anthropic"
delegation = "on_demand"
integration = "O root Claude Opus 5 medium recebe os resultados, integra as alterações e verifica a entrega contra o pedido e os testes pertinentes. Somente quando restarem decisões relevantes de arquitetura, riscos ou necessidade de uma segunda análise, aciona o reviewer Claude Opus 5 high. O root resolve os achados e entrega o resultado final ao usuário."
notes = "Equipe inteiramente Claude, executada pelo Claude Code CLI com sua autenticação existente. Todos os papéis usam o identificador explícito claude-opus-5. Delegação sob demanda: iniciar somente agentes com trabalho útil e delimitado; não ativar todos os papéis automaticamente. Respeitar o limite de concorrência configurado no StackPulse (20 subagentes na configuração solicitada) e os limites do CLI. Este perfil não altera o limite global de agentes. A imagem herdada do Astra Quality Team é somente referência de organização; os modelos e esforços executáveis são os definidos neste TOML."

[team.orchestrator]
role = "root"
model = "claude-opus-5"
effort = "medium"
purpose = "Entender o pedido, distribuir trabalho útil entre os papéis disponíveis, integrar e verificar os resultados e responder ao usuário."
when = "Durante a coordenação inicial, acompanhamento, integração e verificação final."

[[team.agents]]
role = "researcher"
model = "claude-opus-5"
effort = "medium"
purpose = "Realizar pesquisa focada e delimitada. Entregar achados verificáveis com fontes ou referências a arquivos, incertezas e implicações para a tarefa."
when = "Sob demanda, quando uma dúvida concreta exigir investigação independente útil para a entrega."

[[team.agents]]
role = "worker"
model = "claude-opus-5"
effort = "high"
purpose = "Implementar código, investigar e corrigir bugs dentro dos arquivos e responsabilidades atribuídos. Executar verificações pertinentes e informar mudanças, resultados e limitações."
when = "Sob demanda, quando houver implementação ou depuração com escopo delimitado a delegar."

[[team.agents]]
role = "writer"
model = "claude-opus-5"
effort = "medium"
purpose = "Produzir textos, documentação e resumos claros a partir de informações verificadas. Preservar fatos, atribuições e requisitos do pedido."
when = "Sob demanda, quando escrita ou síntese constituir uma parte útil e separável da entrega; aguardar os resultados necessários antes de resumir."

[[team.agents]]
role = "reviewer"
model = "claude-opus-5"
effort = "high"
purpose = "Realizar uma revisão independente de arquitetura e da entrega integrada, apontando problemas concretos com evidências e verificações pertinentes."
when = "Somente se necessário, após integração e verificação pelo root, quando houver decisões relevantes de arquitetura, riscos ou necessidade de uma segunda análise."
+++


# Claude Quality Team

Equipe inteiramente **Claude Opus 5**, com delegação sob demanda e revisão
independente somente quando necessária, após a integração pelo root.

| Papel | Modelo | Esforço | Responsabilidade |
|---|---|---|---|
| root | claude-opus-5 | medium | Orquestrar, integrar e verificar |
| researcher | claude-opus-5 | medium | Pesquisa focada |
| worker | claude-opus-5 | high | Código, depuração e verificações |
| writer | claude-opus-5 | medium | Escrita, documentação e resumos |
| reviewer | claude-opus-5 | high | Revisão independente quando necessária |

O root executa tarefas simples diretamente e delega partes úteis e delimitadas.
Aguarda os resultados, integra e verifica a entrega. Quando houver revisão,
resolve os achados antes de entregar o resultado final.

## Execução

O provider `anthropic` seleciona o **Claude Code CLI**. O StackPulse transmite
modelo e esforço do root e registra os quatro papéis nativos com seus próprios
modelos e esforços. Requer Claude Code instalado e autenticado, com acesso ao
modelo configurado. O limite de concorrência vem das configurações do StackPulse;
o perfil não altera esse valor.

Disponível no catálogo `all`, para projetos que utilizem este diretório de
providers. Selecione **claude_quality_team** no seletor de perfis do StackPulse.

A imagem `astra_quality_team.png` é reutilizada como referência da organização
original, exigida pelo formato de perfis. Ela não representa os modelos desta
equipe; a configuração executável está no TOML acima.

## Referências

- [Identificador do Claude Opus 5](https://support.claude.com/en/articles/11940350-claude-code-model-configuration)
- [Modelos e esforços por subagente](https://code.claude.com/docs/en/subagents)
