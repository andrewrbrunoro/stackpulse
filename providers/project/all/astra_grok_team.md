+++
schema_version = 1
source_image = "astra_quality_team.png"
source_sha256 = "436eed15a6c938e5b6419f23860dbd1782fcb15b754d950c5c2497274b1c75ef"
generated_by = "stackpulse:user-request"
generated_at = "2026-09-15T18:44:08.884573Z"

[team]
name = "astra_grok_team"
provider = "openai"
delegation = "on_demand"
integration = "O root GPT-6 Astra medium recebe os resultados dos especialistas Grok, integra e verifica a entrega contra o pedido e os testes pertinentes. Somente quando necessário, aciona o reviewer Grok high para revisão independente. O root resolve os achados e entrega o resultado final."
notes = "GPT-6 Astra como orquestrador openai e Grok 4.6 nos quatro papéis xai. A ponte StackPulse encaminha cada papel ao CLI correspondente com o modelo e esforço configurados. Exige Codex CLI e Grok CLI instalados; autenticação e acesso aos modelos são conferidos pelo CLI ao executar, sem login ou inferência de validação realizados neste perfil. Não substituir silenciosamente modelos. A imagem herdada do perfil Astra Quality Team é somente referência de organização. Delegação sob demanda e limite de concorrência configurado no StackPulse."

[team.orchestrator]
role = "root"
model = "gpt-6-astra"
effort = "medium"
purpose = "Entender o pedido, distribuir trabalho útil entre os papéis disponíveis, integrar e verificar os resultados e responder ao usuário."
when = "Durante a coordenação inicial, acompanhamento, integração e verificação final."

[[team.agents]]
provider = "xai"
role = "researcher"
model = "grok-4.6"
effort = "medium"
purpose = "Realizar pesquisa focada e delimitada. Entregar achados verificáveis com fontes ou referências a arquivos, incertezas e implicações para a tarefa."
when = "Sob demanda, quando uma dúvida concreta exigir investigação independente útil para a entrega."

[[team.agents]]
provider = "xai"
role = "worker"
model = "grok-4.6"
effort = "high"
purpose = "Implementar código, investigar e corrigir bugs dentro dos arquivos e responsabilidades atribuídos. Executar verificações pertinentes e informar mudanças, resultados e limitações."
when = "Sob demanda, quando houver implementação ou depuração com escopo delimitado a delegar."

[[team.agents]]
provider = "xai"
role = "writer"
model = "grok-4.6"
effort = "medium"
purpose = "Produzir textos, documentação e resumos claros a partir de informações verificadas. Preservar fatos, atribuições e requisitos do pedido."
when = "Sob demanda, quando escrita ou síntese constituir uma parte útil e separável da entrega; aguardar os resultados necessários antes de resumir."

[[team.agents]]
provider = "xai"
role = "reviewer"
model = "grok-4.6"
effort = "high"
purpose = "Realizar uma revisão independente de arquitetura e da entrega integrada, apontando problemas concretos com evidências e verificações pertinentes."
when = "Somente se necessário, após integração e verificação pelo root, quando houver decisões relevantes de arquitetura, riscos ou necessidade de uma segunda análise."
+++

# Astra + Grok Team

Perfil solicitado com **GPT-6 Astra no comando** e **Grok 4.6 nos demais papéis**.

| Papel | Provedor | Modelo | Esforço |
|---|---|---|---|
| root | OpenAI / Codex | gpt-6-astra | medium |
| researcher | xAI / Grok CLI | grok-4.6 | medium |
| worker | xAI / Grok CLI | grok-4.6 | high |
| writer | xAI / Grok CLI | grok-4.6 | medium |
| reviewer | xAI / Grok CLI | grok-4.6 | high |

A delegação ocorre sob demanda. O root distribui trabalho útil, aguarda os
resultados, integra e verifica. O reviewer participa somente quando a entrega
precisa de uma segunda análise. O limite de agentes vem das configurações do
Stackpulse.

## Execução com múltiplos providers

O root usa Codex CLI e os papéis auxiliares usam Grok CLI por meio da ponte
StackPulse. O provedor da equipe é o fallback `openai`; cada especialista declara
`provider = "xai"`. A ponte aplica modelo e esforço por papel e aguarda o
resultado para integração pelo root. Ambos os CLIs precisam estar instalados e
autenticados. Este perfil não comprova login nem acesso aos modelos; nenhum
pedido de inferência foi realizado para validar as contas. As métricas dos
providers auxiliares ainda não são consolidadas no total da execução.

## Referências locais

- Organização e responsabilidades: `astra_quality_team.md`.
- `astra_quality_team.png` é a referência herdada de organização; não retrata
  esta combinação de modelos.
- Grok CLI 1.0.13: catálogo `default_models` embutido no executável
  `~/.grok/downloads/grok-1.0.13-macos-aarch64`, consultado em 2026-09-15.
  Anuncia `grok-4.6` e esforços `low`, `medium`, `high`, `xhigh`.
  O help anuncia `--model` e `--reasoning-effort`.
- O ID Astra foi preservado do perfil existente. Nenhuma inferência foi rodada
  para verificar acesso da conta aos modelos.
