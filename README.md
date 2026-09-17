# StackPulse

Organize equipes de agentes de IA, acompanhe o trabalho e compare os resultados no terminal. O StackPulse reúne **Codex CLI, Claude Code, Cursor Agent e Grok CLI** em uma interface, usando o login já configurado em cada ferramenta.

## Por que existe

Trabalhar com diferentes agentes envolve escolher modelos, definir quem faz cada parte e acompanhar o resultado. Para saber qual equipe funciona melhor no seu projeto, também é preciso relacionar a entrega ao tempo e ao consumo de tokens.

O StackPulse reúne esse fluxo: você escolhe um perfil de equipe, envia o pedido e acompanha os agentes em uma conversa. O histórico e as avaliações ficam salvos localmente para consultar e comparar depois.

## O que você pode fazer

- **Montar equipes reutilizáveis.** Defina papéis para pesquisa, implementação e revisão, combinando provedores com um orquestrador Codex ou Claude.
- **Acompanhar o trabalho.** Organize conversas em abas e consulte a atividade dos agentes, o tempo e os tokens disponíveis em cada execução.
- **Comparar resultados.** Execute a mesma tarefa com diferentes perfis em cópias separadas do projeto e compare as entregas validadas por testes com o tempo e o consumo observados.
- **Aprender com o histórico.** Registre avaliações das entregas e acompanhe as tendências de cada perfil ao longo do tempo.

Você também pode partir de uma imagem que descreve uma equipe e transformá-la em um perfil executável. O suporte a equipes e a cobertura das métricas variam por provedor; os guias detalham os limites atuais.

![Chat do StackPulse com tokens, nota e agentes, em ambiente temporário](docs/assets/terminal-widgets.png)

## Início rápido

É necessário ter Rust/Cargo e o CLI do provedor que deseja usar instalados, com o login do CLI configurado.

Na pasta deste repositório:

```sh
./setup.sh
```

O comando `stackpulse` é instalado em `~/.local/bin`, sem `sudo`. Em seguida, entre na pasta do projeto em que deseja trabalhar e inicie:

```sh
cd /caminho/do/seu/projeto
stackpulse
```

Autorize o acesso à pasta e escolha um perfil. Os pedidos executam nessa pasta.

## Documentação

| Guia | Conteúdo |
| --- | --- |
| [Interface no terminal](docs/terminal-interface.md) | Chat, abas, widgets e acompanhamento dos agentes |
| [Da imagem à execução](docs/image-workflows.md) | Setup, perfis por imagem e envio de pedidos |
| [Equipes com múltiplos provedores](docs/multi-provider-teams.md) | Ponte MCP, papéis mistos e limites atuais |
| [Referência de uso](docs/cli-reference.md) | Instalação detalhada, comandos e métricas |

## Projetos relacionados

O instalador oferece [AI-Memory](https://github.com/akitaonrails/ai-memory) e [AI-UsageBar](https://github.com/akitaonrails/ai-usagebar), projetos de [Fabio Akita (AkitaOnRails)](https://akitaonrails.com), como complementos opcionais. A conexão da memória aos agentes é configurada separadamente.
