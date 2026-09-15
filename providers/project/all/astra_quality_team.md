+++
schema_version = 1
source_image = "astra_quality_team.png"
source_sha256 = "436eed15a6c938e5b6419f23860dbd1782fcb15b754d950c5c2497274b1c75ef"
generated_by = "stackpulse:manual-adaptation"
generated_at = "2026-09-15T03:55:49.421061Z"

[team]
name = "astra_quality_team"
provider = "openai"
delegation = "on_demand"
integration = "O root GPT-6 Astra medium recebe os resultados, integra as alterações e verifica a entrega contra o pedido e os testes pertinentes. Somente quando restarem decisões relevantes de arquitetura, riscos ou necessidade de uma segunda análise, aciona o reviewer GPT-6 Astra high. O root resolve os achados e entrega o resultado final ao usuário."
notes = "Adaptação da imagem fornecida pelo usuário. A imagem original foi preservada como referência: seus três DeepSeek V4.1 Flash foram substituídos por GPT-6 Astra nesta configuração executável. Prioridade: capacidade e compatibilidade com o Codex CLI instalado. A equipe atual usa um único client; este perfil não mistura assinaturas de Codex e Claude. Não há garantia de menor custo, menor latência ou superioridade ao DeepSeek em todas as tarefas; compare entregas equivalentes antes de concluir. Os esforços dos três especialistas foram definidos na adaptação, pois a imagem não os informa. Delegue somente trabalho útil: não inicie todos os papéis automaticamente, evite repetir pesquisa e forneça contexto delimitado. Esta configuração não altera o limite global de agentes."

[team.orchestrator]
role = "root"
model = "gpt-6-astra"
effort = "medium"
purpose = "Entender o pedido, distribuir trabalho útil entre os papéis disponíveis, integrar e verificar os resultados e responder ao usuário."
when = "Durante a coordenação inicial, acompanhamento, integração e verificação final."

[[team.agents]]
role = "researcher"
model = "gpt-6-astra"
effort = "medium"
purpose = "Realizar pesquisa focada e delimitada. Entregar achados verificáveis com fontes ou referências a arquivos, incertezas e implicações para a tarefa."
when = "Sob demanda, quando uma dúvida concreta exigir investigação independente útil para a entrega."

[[team.agents]]
role = "worker"
model = "gpt-6-astra"
effort = "high"
purpose = "Implementar código, investigar e corrigir bugs dentro dos arquivos e responsabilidades atribuídos. Executar verificações pertinentes e informar mudanças, resultados e limitações."
when = "Sob demanda, quando houver implementação ou depuração com escopo delimitado a delegar."

[[team.agents]]
role = "writer"
model = "gpt-6-astra"
effort = "medium"
purpose = "Produzir textos, documentação e resumos claros a partir de informações verificadas. Preservar fatos, atribuições e requisitos do pedido."
when = "Sob demanda, quando escrita ou síntese constituir uma parte útil e separável da entrega; aguardar os resultados necessários antes de resumir."

[[team.agents]]
role = "reviewer"
model = "gpt-6-astra"
effort = "high"
purpose = "Realizar uma revisão independente de arquitetura e da entrega integrada, apontando problemas concretos com evidências e verificações pertinentes."
when = "Somente se necessário, após integração e verificação pelo root, quando houver decisões relevantes de arquitetura, riscos ou necessidade de uma segunda análise."
+++

# Astra Quality Team

Adaptação do fluxo **Astra + Flash** da imagem fornecida. A configuração executável
substitui os três DeepSeek V4.1 Flash por GPT-6 Astra, mantendo os papéis e a
delegação sob demanda. A imagem original está preservada em `astra_quality_team.png`.

| Papel | Modelo | Esforço | Responsabilidade |
|---|---|---|---|
| root | gpt-6-astra | medium | Orquestrar, integrar e verificar |
| researcher | gpt-6-astra | medium | Pesquisa focada |
| worker | gpt-6-astra | high | Código, depuração e verificações |
| writer | gpt-6-astra | medium | Escrita, documentação e resumos |
| reviewer | gpt-6-astra | high | Arquitetura e revisão final, somente se necessário |

O orquestrador decide quais papéis ajudam no pedido. Os três especialistas não são
etapas obrigatórias. A integração volta ao root; a revisão independente é opcional.
Os esforços dos especialistas são escolhas desta adaptação, ausentes na imagem.

Este perfil usa o **Codex CLI** e sua autenticação existente. A seleção está
disponível no início de uma conversa e em **Configurações → Perfil**. O escopo
`all` permite usá-lo em qualquer projeto que utilize este catálogo de providers.

A escolha prioriza capacidade: a OpenAI apresenta
[GPT-6 Astra como seu modelo mais capaz para trabalho complexo](https://developers.openai.com/api/docs/models/gpt-6-astra).
Isso não comprova superioridade ao DeepSeek V4.1 Flash em toda tarefa, nem mantém
necessariamente a economia sugerida pelo título da imagem. Custo, latência e
qualidade devem ser comparados usando o mesmo pedido, ambiente e critérios de entrega.

Combinar Astra como root e Claude como especialista requer suporte adicional a
equipes com mais de um client no executor do StackPulse. Este perfil usa apenas
modelos nativos do Codex e não declara esse suporte como implementado.
