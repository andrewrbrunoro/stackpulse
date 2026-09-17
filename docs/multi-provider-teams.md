# Equipes com múltiplos provedores

O campo `team.provider` define o provedor padrão. `provider` no orquestrador ou em um papel substitui esse padrão somente para aquele agente. Perfis existentes sem esse campo continuam usando um provedor único.

Exemplo de trechos do TOML de um perfil:

```toml
[team]
provider = "openai"

[team.orchestrator]
role = "root"
model = "gpt-6-astra"
effort = "medium"
# Inclua purpose e when como nos perfis existentes.

[[team.agents]]
role = "worker"
provider = "xai"
model = "grok-4.6"
effort = "high"
# Inclua purpose e when como nos perfis existentes.
```

O perfil completo precisa dos demais campos usuais, como nome, delegação e integração. `providers/project/all/astra_grok_team.md` traz a configuração completa com os quatro especialistas Grok.

## Execução

A ponte está disponível em macOS e Linux; no Windows, a execução mista é recusada porque o encerramento da árvore de processos ainda não foi implementado. Equipes mistas aceitam orquestrador Codex ou Claude. Os filhos usam os adaptadores Codex, Claude ou Grok conforme o provedor de cada papel. O CLI correspondente precisa estar instalado, autenticado e ter acesso ao modelo solicitado. A descoberta local de executáveis não comprova login nem acesso ao modelo.

A ponte MCP `stackpulse_team` é criada por execução; as configurações salvas dos CLIs não são alteradas. O orquestrador usa `spawn`, `status`, `wait` e `cancel`. Cada tarefa recebe o contexto explicitamente enviado pelo root, sem retomar a sessão nativa de outro CLI. O resultado retorna ao orquestrador para integração. Falhas dos filhos são retornadas como falhas, sem substituição automática de modelo ou provedor.

A ponte impõe o limite de filhos ativos; `sequential` permite somente um por vez. Os filhos recebem o diretório e o modo de permissão da execução e não podem criar subagentes pela configuração da ponte. As políticas de permissão específicas de cada adaptador continuam valendo.

## Limites atuais

- O consumo dos filhos externos ainda não é agregado às métricas da conversa. Equipes mistas mantêm cobertura parcial, inclusive depois da atualização dos logs locais.
- Os cartões e controles de orientação/redirecionamento de subagentes nativos não representam os filhos da ponte. O orquestrador acompanha essas tarefas pelas ferramentas MCP.
- Cursor ainda não pode ser um filho da ponte: seu adaptador não desativa a delegação nativa, necessária para garantir o limite de agentes.
- O suporte a uma equipe mista não habilita root Grok ou Cursor com subagentes. Esses adaptadores continuam recusando essa configuração.
- A disponibilidade dos modelos depende das contas e CLIs instalados. Os testes automatizados usam executáveis simulados e não fazem inferências pagas.
