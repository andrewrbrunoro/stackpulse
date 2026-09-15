+++
schema_version = 1
source_image = "cost_efficient_gpt.png"
source_sha256 = "254ecee39b7a4d619883db897569f6bfb459e5eaecf18fcdc2d452e849d4c00c"
generated_by = "openai:gpt-6-astra:medium"
generated_at = "2026-09-12T17:26:22.698355Z"

[team]
name = "cost_efficient_gpt"
provider = "openai"
delegation = "on_demand"
integration = "O agente root, gpt-6-astra com esforço medium, integra e verifica os resultados delegados. Após essa etapa, pode haver revisão independente, somente se necessário."
notes = "Provider openai inferido da família GPT/Astra/Sol/Luna. Os papéis são acionados sob demanda; a imagem não determina ativação de todos nem quantidade de agentes por papel."

[team.orchestrator]
role = "root"
model = "gpt-6-astra"
effort = "medium"
purpose = "Orquestrar, delegar trabalho útil sob demanda, integrar e verificar resultados."
when = "Na coordenação inicial e na integração e verificação dos resultados."

[[team.agents]]
role = "explorer"
model = "gpt-5.6-luna"
effort = "max"
purpose = "Investigação delimitada."
when = "Sob demanda, quando houver investigação útil a delegar."

[[team.agents]]
role = "worker"
model = "gpt-5.6-sol"
effort = "high"
purpose = "Implementação e testes."
when = "Sob demanda, quando houver implementação útil a delegar."

[[team.agents]]
role = "researcher"
model = "gpt-5.6-luna"
effort = "max"
purpose = "Consulta focada."
when = "Sob demanda, quando houver consulta útil a delegar."

[[team.agents]]
role = "reviewer"
model = "gpt-6-astra"
effort = "xhigh"
purpose = "Revisão independente."
when = "Somente se necessário, após integração e verificação."
+++

# cost_efficient_gpt

Perfil extraído de `cost_efficient_gpt.png`. A configuração executável está no bloco TOML acima.

| Papel | Modelo | Esforço | Quando |
|---|---|---|---|
| root | gpt-6-astra | medium | Na coordenação inicial e na integração e verificação dos resultados. |
| explorer | gpt-5.6-luna | max | Sob demanda, quando houver investigação útil a delegar. |
| worker | gpt-5.6-sol | high | Sob demanda, quando houver implementação útil a delegar. |
| researcher | gpt-5.6-luna | max | Sob demanda, quando houver consulta útil a delegar. |
| reviewer | gpt-6-astra | xhigh | Somente se necessário, após integração e verificação. |

Delegação: `on_demand`.

Integração: O agente root, gpt-6-astra com esforço medium, integra e verifica os resultados delegados. Após essa etapa, pode haver revisão independente, somente se necessário.

Provider openai inferido da família GPT/Astra/Sol/Luna. Os papéis são acionados sob demanda; a imagem não determina ativação de todos nem quantidade de agentes por papel.
