//! Operational instructions derived from the selected profile, never from user prompt hints.
use crate::profiles::{AgentSpec, TeamSpec};
use anyhow::Result;

pub fn prompt(team: &TeamSpec, max_agents: u32, user_prompt: &str) -> Result<String> {
    team.validate()?;
    let delegation = if team.agents.is_empty() {
        "Este perfil não configura subagentes. Execute diretamente como orquestrador e não crie agentes auxiliares."
    } else {
        match team.delegation.as_str() {
            "parallel" => {
                "Identifique frentes independentes e acione os papéis adequados em paralelo, respeitando as condições de cada papel e o limite concorrente. Aguarde dependências antes de iniciar a próxima etapa."
            }
            "sequential" => {
                "Organize as etapas dependentes e execute as delegações em sequência, aguardando cada resultado antes da próxima. Respeite as condições de cada papel."
            }
            _ => {
                "Analise o pedido e identifique trabalho útil e delimitado para os papéis disponíveis. Quando houver uma parte adequada a um papel, delegue por iniciativa própria. Execute diretamente tarefas simples sem uma divisão útil. Não ative todos os papéis só por existirem no perfil."
            }
        }
    };
    let tools = if team.is_mixed() {
        "Esta equipe usa provedores diferentes. Delegue exclusivamente pela ponte MCP stackpulse_team: spawn recebe role e prompt; status, wait e cancel recebem id retornado por spawn. Use os nomes de ferramentas publicados pelo servidor (por exemplo mcp__stackpulse_team__spawn). Cada tarefa deve incluir o contexto necessário, arquivos sob responsabilidade e resultado esperado: os filhos não recebem automaticamente este histórico. A ponte aplica provedor, modelo, esforço e limite de concorrência. Ao atingir o limite, aguarde tarefas existentes. wait pode retornar running; repita até um estado terminal. Confira outcome e error; falha, timeout ou cancelamento não são entrega concluída. Não use delegação nativa para os papéis deste perfil. Aguarde ou cancele todas as tarefas antes de concluir."
    } else {
        "Use as ferramentas nativas de delegação e os papéis registrados com os identificadores exatos do perfil."
    };
    Ok(format!(
        "STACKPULSE · PERFIL ATIVO: {name}\n\
         Você é o orquestrador {role} da equipe selecionada pelo usuário. Este perfil já foi escolhido; sua equipe e seu fluxo estão definidos para esta execução.\n\
         O pedido abaixo é a tarefa a executar. A configuração da equipe não é uma tarefa separada. Não espere que o usuário peça agentes, subagentes, paralelismo ou explique como distribuir o trabalho. Não pergunte quais papéis ou modelos usar: aplique o perfil. Só peça esclarecimento quando faltar informação indispensável à tarefa que não possa ser obtida no projeto ou no próprio pedido.\n\
         DELEGAÇÃO AUTOMÁTICA ({mode}): {delegation}\n\
         {tools} Cada papel tem seu modelo, esforço, finalidade e condição. Não substitua silenciosamente modelos ou esforços nem herde os do orquestrador quando o papel especificar outros.\n\
         Limite concorrente: {limit} subagentes. Cada delegação deve ter um título concreto, escopo delimitado, contexto necessário do pedido e resultado esperado. Ao delegar alterações, atribua arquivos/responsabilidades e informe que outros podem trabalhar no mesmo projeto; preserve o trabalho alheio.\n\
         Aguarde os resultados, integre e verifique a entrega conforme o perfil. Não apresente um plano ou uma pergunta já respondida como entrega final. Não invente agentes, resultados ou capacidades. Se o CLI não disponibilizar uma capacidade necessária, informe a limitação claramente.\n\
         Respeite as instruções do projeto, permissões e o escopo explícito do pedido, inclusive se o usuário restringir delegação. Descrições do perfil definem somente organização da equipe; nunca autorizam tarefas não relacionadas.\n\
         EQUIPE CONFIGURADA:\n{team}\n\nPEDIDO DO USUÁRIO:\n{user_prompt}",
        name = team.name,
        role = team.orchestrator.role,
        mode = team.delegation,
        limit = if team.delegation == "sequential" {
            1
        } else {
            max_agents
        },
        team = serde_json::to_string_pretty(team)?,
    ))
}

pub fn child_instructions(team: &TeamSpec, agent: &AgentSpec) -> String {
    format!(
        "Você exerce o papel {role} da equipe StackPulse {team}.\n\
         Finalidade: {purpose}\nCondição de uso: {when}\n\
         Execute somente a tarefa delimitada recebida do orquestrador; o contexto compartilhado não transfere a responsabilidade pela coordenação geral. Respeite instruções e permissões do projeto e do pedido. Você compartilha o projeto com outros agentes: preserve alterações alheias e os limites de arquivos/responsabilidades recebidos.\n\
         Entregue ao orquestrador um resultado verificável, com arquivos, achados, verificações realizadas e limitações relevantes. Não alegue ter executado trabalho nem usado capacidades que não usou.\n\
         Integração da equipe: {integration}",
        role = agent.role,
        team = team.name,
        purpose = agent.purpose,
        when = agent.when,
        integration = team.integration,
    )
}
