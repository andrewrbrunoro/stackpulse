//! Forms translate directly to our existing CLI; no shell parses user input.

use anyhow::{Context, Result, ensure};
use chrono::{DateTime, Utc};
use std::{ffi::OsString, path::PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Action {
    Run,
    AddProfile,
    CompileProfile,
    Feedback,
    Tag,
    Price,
    Import,
    Report,
    Trend,
    Widget,
    Sync,
    Setup,
}

#[derive(Clone, Debug)]
pub(crate) struct Field {
    pub label: String,
    pub hint: String,
    pub value: String,
    pub required: bool,
    pub multiline: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct Form {
    pub action: Action,
    pub title: String,
    pub description: String,
    pub fields: Vec<Field>,
}

#[derive(Debug)]
pub(crate) struct Invocation {
    pub args: Vec<OsString>,
    pub interactive: bool,
    pub title: String,
    pub export_path: Option<PathBuf>,
}

fn field(label: &str, hint: &str, value: &str, required: bool) -> Field {
    Field {
        label: label.into(),
        hint: hint.into(),
        value: value.into(),
        required,
        multiline: false,
    }
}

fn multiline(label: &str, hint: &str, required: bool) -> Field {
    Field {
        multiline: true,
        ..field(label, hint, "", required)
    }
}

pub(crate) fn form(action: Action, selected: Option<&str>) -> Form {
    let selected = selected.unwrap_or_default();
    let (title, description, fields) = match action {
        Action::Run => (
            "Executar pedido",
            "Usa o perfil escolhido e registra stack, tokens e tempo. A execução pode consumir seu plano.",
            vec![
                multiline(
                    "Pedido",
                    "Descreva a entrega; colagens preservam as quebras de linha.",
                    true,
                ),
                field(
                    "Perfil",
                    "Vazio: perfil padrão ou único encontrado neste projeto.",
                    selected,
                    false,
                ),
                field(
                    "Benchmark",
                    "Opcional: nome de uma tarefa repetível para comparar execuções.",
                    "",
                    false,
                ),
                field(
                    "Permissões",
                    "workspace-write permite editar o projeto; read-only só leitura.",
                    "workspace-write",
                    true,
                ),
                field(
                    "Limite em segundos",
                    "De 1 a 86400; ao atingir o limite, a execução é encerrada e registrada.",
                    "1800",
                    true,
                ),
                field(
                    "Somente prévia",
                    "sim = mostrar instruções; não = executar o pedido no provider.",
                    "não",
                    true,
                ),
            ],
        ),
        Action::AddProfile => (
            "Adicionar imagem de equipe",
            "Copia a imagem para providers/project/escopo e pode gerar o perfil Markdown com seu auxiliar.",
            vec![
                field(
                    "Imagem",
                    "Caminho de PNG, JPG, JPEG ou WEBP; pode usar ~/ e espaços, sem aspas.",
                    "",
                    true,
                ),
                field(
                    "Escopo",
                    "all disponibiliza a equipe para todos os projetos; ou informe o nome do projeto.",
                    "all",
                    true,
                ),
                field(
                    "Nome",
                    "Opcional: letras, números, hífen ou sublinhado; vazio usa o nome da imagem.",
                    "",
                    false,
                ),
                field(
                    "Apenas copiar",
                    "sim = adiar geração; não = gerar Markdown com o auxiliar agora.",
                    "não",
                    true,
                ),
            ],
        ),
        Action::CompileProfile => (
            "Gerar perfil Markdown",
            "Seu auxiliar interpreta a imagem e salva a configuração da equipe ao lado dela.",
            vec![
                field(
                    "Imagem",
                    "PNG, JPG, JPEG ou WEBP; informe o caminho sem aspas.",
                    selected,
                    true,
                ),
                field(
                    "Gerar novamente",
                    "sim permite substituir o Markdown existente.",
                    "não",
                    true,
                ),
                field("Limite em segundos", "De 1 a 3600.", "300", true),
            ],
        ),
        Action::Feedback => (
            "Avaliar execução",
            "A avaliação fica junto do consumo e ajuda a comparar entregas ao longo dos dias.",
            vec![
                field(
                    "Execução",
                    "ID completo ou prefixo único de uma execução concluída.",
                    selected,
                    true,
                ),
                field(
                    "Entrega",
                    "De 0 a 1: 0 não entregou, 0,5 entregou parte, 1 entregou tudo.",
                    "",
                    true,
                ),
                field(
                    "Rapidez",
                    "De 1 a 5: 1 muito lenta, 5 muito rápida; 0 não avaliada.",
                    "",
                    true,
                ),
                multiline("Observação", "Opcional; até 4000 bytes de texto.", false),
            ],
        ),
        Action::Tag => (
            "Anotar execução coletada",
            "Associe um benchmark e uma avaliação; grupos comparáveis usam a mesma tarefa e configuração.",
            vec![
                field(
                    "Execução raiz",
                    "ID completo ou prefixo único da execução coletada.",
                    selected,
                    true,
                ),
                field(
                    "Rótulo",
                    "Opcional; vazio mantém o rótulo atual.",
                    "",
                    false,
                ),
                field(
                    "Benchmark",
                    "Opcional; vazio mantém o benchmark atual.",
                    "",
                    false,
                ),
                field(
                    "Qualidade",
                    "Opcional: de 0 a 1; vazio mantém a avaliação atual.",
                    "",
                    false,
                ),
                field(
                    "Grupo",
                    "keep mantém o grupo; baseline define referência; current define comparação.",
                    "keep",
                    true,
                ),
            ],
        ),
        Action::Price => (
            "Cadastrar tarifa",
            "Valores em USD por milhão de tokens, usados para estimativas. Informe tarifas e fonte conhecidas.",
            vec![
                field(
                    "Provider",
                    "Identificador usado nos registros, por exemplo openai.",
                    "",
                    true,
                ),
                field(
                    "Modelo",
                    "Identificador exato do modelo registrado.",
                    "",
                    true,
                ),
                field(
                    "Nível de serviço",
                    "Use o nível registrado; unknown quando não é informado.",
                    "unknown",
                    true,
                ),
                field(
                    "Válida desde",
                    "Data com fuso, por exemplo 2026-09-12T00:00:00-03:00.",
                    &Utc::now().to_rfc3339(),
                    true,
                ),
                field(
                    "Entrada",
                    "USD por milhão de tokens de entrada sem cache.",
                    "",
                    true,
                ),
                field(
                    "Leitura de cache",
                    "USD por milhão de tokens lidos do cache.",
                    "",
                    true,
                ),
                field(
                    "Escrita de cache",
                    "USD por milhão de tokens escritos no cache.",
                    "",
                    true,
                ),
                field("Saída", "USD por milhão de tokens de saída.", "", true),
                field(
                    "Fonte",
                    "Página ou referência da tarifa cadastrada.",
                    "",
                    true,
                ),
            ],
        ),
        Action::Import => (
            "Importar métricas",
            "Importa um arquivo JSON normalizado para o banco local, sem duplicar eventos já registrados.",
            vec![field(
                "Arquivo JSON",
                "Dataset com sessions, usage e turns; annotations e prices são opcionais.",
                "",
                true,
            )],
        ),
        Action::Report => (
            "Relatório de consumo",
            "Mostra o relatório em JSON; você pode salvar uma cópia em um arquivo novo.",
            vec![
                field(
                    "Período",
                    "hour = hora; day = dia; month = mês.",
                    "day",
                    true,
                ),
                field(
                    "Projeto",
                    "Opcional: caminho da pasta do projeto.",
                    "",
                    false,
                ),
                field("Execução raiz", "Opcional: ID ou prefixo único.", "", false),
                field(
                    "Salvar em",
                    "Opcional: caminho de um novo arquivo JSON; vazio mostra só na interface.",
                    "",
                    false,
                ),
            ],
        ),
        Action::Trend => (
            "Tendência de entregas",
            "Agrupa execuções comparáveis e suaviza as avaliações diárias; mudanças não comprovam alteração do provider.",
            vec![
                field("Dias", "Janela entre 2 e 366 dias.", "30", true),
                field(
                    "Suavização",
                    "Alpha maior que 0 e até 1; valores menores deixam a linha mais suave.",
                    "0,3",
                    true,
                ),
                field("Perfil", "Opcional: filtra pelo nome do perfil.", "", false),
                field("Benchmark", "Opcional: filtra pelo benchmark.", "", false),
            ],
        ),
        Action::Widget => (
            "Abrir widget compacto",
            "Acompanhe o consumo ao vivo; h/d/m mudam o período, g mostra avaliações e q volta ao menu.",
            vec![
                field(
                    "Período",
                    "hour = hora; day = dia; month = mês.",
                    "day",
                    true,
                ),
                field(
                    "Projeto",
                    "Opcional: caminho da pasta do projeto.",
                    "",
                    false,
                ),
                field("Execução raiz", "Opcional: ID ou prefixo único.", "", false),
                field(
                    "Iniciar em avaliações",
                    "sim abre a curva de entrega; não abre o consumo.",
                    "não",
                    true,
                ),
                field(
                    "Atualização em segundos",
                    "De 1 a 3600; sincroniza as sessões locais do Codex.",
                    "5",
                    true,
                ),
            ],
        ),
        Action::Sync => (
            "Sincronizar sessões",
            "Coleta as sessões locais do Codex para atualizar o histórico; os outros clientes são registrados ao executar pedidos.",
            vec![],
        ),
        Action::Setup => (
            "Configurar auxiliar",
            "Abre o setup com os clientes instalados, modelo, limite de agentes e pasta de perfis.",
            vec![],
        ),
    };
    Form {
        action,
        title: title.into(),
        description: description.into(),
        fields,
    }
}

/// Option values use `--name=value`; positionals follow `--` so neither can
/// become an extra command-line flag, even when starting with a hyphen.
pub(crate) fn build(form: &Form) -> Result<Invocation> {
    let expected = self::form(form.action, None).fields;
    ensure!(form.fields.len() == expected.len(), "Formulário incompleto");
    for (field, spec) in form.fields.iter().zip(&expected) {
        ensure!(
            !spec.required || !field.value.trim().is_empty(),
            "Preencha {}",
            spec.label
        );
        ensure!(
            !field.value.contains('\0'),
            "{} contém um caractere nulo",
            spec.label
        );
        ensure!(
            spec.multiline || !field.value.chars().any(char::is_control),
            "{} deve ocupar uma linha",
            spec.label
        );
    }
    let value = |i: usize| form.fields[i].value.trim();
    let mut args = Vec::new();
    let mut export_path = None;
    match form.action {
        Action::Run => {
            ensure!(
                form.fields[0].value.len() <= 1_000_000,
                "Pedido maior que 1 MB"
            );
            ensure!(
                value(2).len() <= 300,
                "Benchmark deve ter até 300 bytes de texto"
            );
            if !value(1).is_empty() {
                crate::profiles::identifier(value(1))?;
            }
            choice(value(3), &["read-only", "workspace-write"], "Permissões")?;
            integer(value(4), 1, 86400, "Limite em segundos")?;
            args.push("run".into());
            optional(&mut args, "profile", value(1));
            optional(&mut args, "benchmark", value(2));
            option(&mut args, "sandbox", value(3));
            option(&mut args, "timeout", value(4));
            flag(&mut args, "dry-run", boolean(value(5), "Somente prévia")?);
            flag(&mut args, "no-feedback", true);
            positional(&mut args, form.fields[0].value.clone().into());
        }
        Action::AddProfile => {
            let image = image_path(value(0))?;
            crate::profiles::scope_name(value(1))?;
            let name = if value(2).is_empty() {
                image
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .context("A imagem precisa de um nome válido")?
            } else {
                value(2)
            };
            crate::profiles::identifier(name)
                .context("Informe um nome de perfil com letras, números, hífen ou sublinhado")?;
            args.extend(["profiles".into(), "add".into()]);
            option(&mut args, "scope", value(1));
            optional(&mut args, "name", value(2));
            flag(&mut args, "no-compile", boolean(value(3), "Apenas copiar")?);
            positional(&mut args, image.into_os_string());
        }
        Action::CompileProfile => {
            let image = image_path(value(0))?;
            integer(value(2), 1, 3600, "Limite em segundos")?;
            args.extend(["profiles".into(), "compile".into()]);
            flag(&mut args, "force", boolean(value(1), "Gerar novamente")?);
            option(&mut args, "timeout", value(2));
            positional(&mut args, image.into_os_string());
        }
        Action::Feedback => {
            let delivered = decimal(value(1), 0.0, 1.0, "Entrega")?;
            integer(value(2), 0, 5, "Rapidez")?;
            ensure!(
                form.fields[3].value.len() <= 4000,
                "Observação deve ter até 4000 bytes de texto"
            );
            args.push("feedback".into());
            option(&mut args, "delivered", &delivered.to_string());
            option(&mut args, "speed", value(2));
            if !form.fields[3].value.is_empty() {
                option(&mut args, "note", &form.fields[3].value);
            }
            positional(&mut args, value(0).into());
        }
        Action::Tag => {
            choice(value(4), &["keep", "baseline", "current"], "Grupo")?;
            args.push("tag".into());
            optional(&mut args, "label", value(1));
            optional(&mut args, "benchmark", value(2));
            if !value(3).is_empty() {
                let quality = decimal(value(3), 0.0, 1.0, "Qualidade")?;
                option(&mut args, "quality", &quality.to_string());
            }
            flag(&mut args, "baseline", value(4) == "baseline");
            flag(&mut args, "current", value(4) == "current");
            positional(&mut args, value(0).into());
        }
        Action::Price => {
            DateTime::parse_from_rfc3339(value(3)).context(
                "Válida desde precisa de data e fuso, por exemplo 2026-09-12T00:00:00-03:00",
            )?;
            args.push("price".into());
            for (index, name) in ["provider", "model", "service-tier", "effective-at"]
                .iter()
                .enumerate()
            {
                option(&mut args, name, value(index));
            }
            for (index, name) in ["input", "cached", "cache-write", "output"]
                .iter()
                .enumerate()
            {
                let price = decimal(
                    value(index + 4),
                    0.0,
                    f64::MAX,
                    &form.fields[index + 4].label,
                )?;
                option(&mut args, name, &price.to_string());
            }
            option(&mut args, "source", value(8));
        }
        Action::Import => {
            let path = existing_file(value(0), "Arquivo JSON")?;
            let file =
                std::fs::File::open(&path).context("Não foi possível abrir o arquivo JSON")?;
            serde_json::from_reader::<_, crate::model::Dataset>(std::io::BufReader::new(file))
                .context(
                    "JSON inválido: informe um Dataset normalizado com sessions, usage e turns",
                )?;
            args.push("import".into());
            positional(&mut args, path.into_os_string());
        }
        Action::Report => {
            choice(value(0), &["hour", "day", "month"], "Período")?;
            args.push("report".into());
            option(&mut args, "period", value(0));
            filters(&mut args, value(1), value(2))?;
            if !value(3).is_empty() {
                let path = expand_path(value(3))?;
                ensure!(
                    !path.exists(),
                    "Salvar em: o arquivo já existe; escolha um novo nome"
                );
                let parent = path
                    .parent()
                    .filter(|p| !p.as_os_str().is_empty())
                    .unwrap_or(std::path::Path::new("."));
                ensure!(parent.is_dir(), "Salvar em: a pasta de destino não existe");
                ensure!(
                    path.file_name().is_some(),
                    "Salvar em: informe um nome de arquivo"
                );
                export_path = Some(path);
            }
        }
        Action::Trend => {
            integer(value(0), 2, 366, "Dias")?;
            let alpha = decimal(value(1), 0.0, 1.0, "Suavização")?;
            ensure!(alpha > 0.0, "Suavização deve ser maior que 0 e até 1");
            args.push("trend".into());
            option(&mut args, "days", value(0));
            option(&mut args, "alpha", &alpha.to_string());
            optional(&mut args, "profile", value(2));
            optional(&mut args, "benchmark", value(3));
        }
        Action::Widget => {
            choice(value(0), &["hour", "day", "month"], "Período")?;
            integer(value(4), 1, 3600, "Atualização em segundos")?;
            args.push("widget".into());
            option(&mut args, "period", value(0));
            filters(&mut args, value(1), value(2))?;
            flag(
                &mut args,
                "quality",
                boolean(value(3), "Iniciar em avaliações")?,
            );
            option(&mut args, "interval", value(4));
        }
        Action::Sync => args.push("sync".into()),
        Action::Setup => args.push("setup".into()),
    }
    Ok(Invocation {
        args,
        interactive: matches!(form.action, Action::Widget | Action::Setup),
        title: form.title.clone(),
        export_path,
    })
}

fn option(args: &mut Vec<OsString>, name: &str, value: &str) {
    args.push(format!("--{name}={value}").into());
}

fn optional(args: &mut Vec<OsString>, name: &str, value: &str) {
    if !value.is_empty() {
        option(args, name, value);
    }
}

fn flag(args: &mut Vec<OsString>, name: &str, enabled: bool) {
    if enabled {
        args.push(format!("--{name}").into());
    }
}

fn positional(args: &mut Vec<OsString>, value: OsString) {
    args.extend(["--".into(), value]);
}

fn boolean(value: &str, label: &str) -> Result<bool> {
    match value.to_lowercase().as_str() {
        "sim" => Ok(true),
        "não" | "nao" => Ok(false),
        _ => anyhow::bail!("{label}: use sim ou não"),
    }
}

fn choice(value: &str, allowed: &[&str], label: &str) -> Result<()> {
    ensure!(
        allowed.contains(&value),
        "{label}: use {}",
        allowed.join(", ")
    );
    Ok(())
}

fn integer(value: &str, min: u64, max: u64, label: &str) -> Result<u64> {
    let number: u64 = value
        .parse()
        .with_context(|| format!("{label}: informe um número inteiro entre {min} e {max}"))?;
    ensure!(
        (min..=max).contains(&number),
        "{label}: use um número entre {min} e {max}"
    );
    Ok(number)
}

fn decimal(value: &str, min: f64, max: f64, label: &str) -> Result<f64> {
    let number: f64 = value
        .replace(',', ".")
        .parse()
        .with_context(|| format!("{label}: informe um número decimal"))?;
    ensure!(
        number.is_finite() && number >= min && number <= max,
        "{label}: valor fora do intervalo permitido"
    );
    Ok(number)
}

fn expand_path(value: &str) -> Result<PathBuf> {
    if value == "~" || value.starts_with("~/") {
        let home = std::env::var_os("HOME")
            .context("Não foi possível resolver ~; informe o caminho completo")?;
        Ok(PathBuf::from(home).join(value.strip_prefix("~/").unwrap_or_default()))
    } else {
        Ok(PathBuf::from(value))
    }
}

fn existing_file(value: &str, label: &str) -> Result<PathBuf> {
    let path = expand_path(value)?;
    ensure!(
        path.is_file(),
        "{label}: arquivo não encontrado: {}",
        path.display()
    );
    Ok(path)
}

fn image_path(value: &str) -> Result<PathBuf> {
    let path = existing_file(value, "Imagem")?;
    let extension = path
        .extension()
        .and_then(|v| v.to_str())
        .unwrap_or_default()
        .to_lowercase();
    ensure!(
        matches!(extension.as_str(), "png" | "jpg" | "jpeg" | "webp"),
        "Imagem: use PNG, JPG, JPEG ou WEBP"
    );
    ensure!(
        std::fs::metadata(&path)?.len() <= 20 * 1024 * 1024,
        "Imagem maior que 20 MB"
    );
    Ok(path)
}

fn filters(args: &mut Vec<OsString>, project: &str, run: &str) -> Result<()> {
    if !project.is_empty() {
        let path = expand_path(project)?;
        // Historical usage remains filterable after a project/worktree was removed.
        let mut argument = OsString::from("--project=");
        argument.push(path);
        args.push(argument);
    }
    optional(args, "run", run);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(invocation: Invocation) -> Vec<String> {
        invocation
            .args
            .into_iter()
            .map(|v| v.into_string().unwrap())
            .collect()
    }

    #[test]
    fn prompts_preserve_multiline_unicode_and_cannot_be_flags_or_shell_code() {
        let mut form = form(Action::Run, Some("equipe"));
        let prompt = "--help\nFaça revisão: $(touch /tmp/never) `pwd` ' 🚀\n";
        form.fields[0].value = prompt.into();
        form.fields[2].value = "--timeout=1; echo literal".into();
        let args = strings(build(&form).unwrap());
        assert_eq!(&args[args.len() - 2..], &["--", prompt]);
        assert!(args.contains(&"--benchmark=--timeout=1; echo literal".into()));
        assert!(args.contains(&"--no-feedback".into()));
        assert_eq!(
            args.iter()
                .filter(|arg| arg.starts_with("--timeout="))
                .count(),
            1
        );
    }

    #[test]
    fn invalid_bounds_and_nonfinite_values_never_create_invocations() {
        let mut feedback = form(Action::Feedback, Some("job-id"));
        feedback.fields[1].value = "0,75".into();
        feedback.fields[2].value = "5".into();
        assert!(strings(build(&feedback).unwrap()).contains(&"--delivered=0.75".into()));
        for invalid in ["NaN", "inf", "-0.1", "1.1"] {
            feedback.fields[1].value = invalid.into();
            assert!(build(&feedback).is_err());
        }
        let mut trend = form(Action::Trend, None);
        for invalid in ["0", "NaN", "1.01"] {
            trend.fields[1].value = invalid.into();
            assert!(build(&trend).is_err());
        }
        let mut run = form(Action::Run, None);
        run.fields[0].value = "Pedido".into();
        for invalid in ["0", "86401", "1.5", "-1"] {
            run.fields[4].value = invalid.into();
            assert!(build(&run).is_err());
        }
        run.fields[4].value = "60".into();
        run.fields[3].value = "unsafe".into();
        assert!(build(&run).is_err());
    }

    #[test]
    fn tag_empty_options_preserve_existing_values_and_group_is_exclusive() {
        let mut form = form(Action::Tag, Some("abc"));
        assert_eq!(strings(build(&form).unwrap()), ["tag", "--", "abc"]);
        form.fields[4].value = "baseline".into();
        let baseline = strings(build(&form).unwrap());
        assert!(baseline.contains(&"--baseline".into()));
        assert!(!baseline.contains(&"--current".into()));
        form.fields[4].value = "current".into();
        let current = strings(build(&form).unwrap());
        assert!(!current.contains(&"--baseline".into()));
        assert!(current.contains(&"--current".into()));
    }

    #[test]
    fn image_paths_with_spaces_are_single_arguments_and_name_is_validated() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("uma imagem.png");
        std::fs::write(&path, b"fixture").unwrap();
        let mut form = form(Action::AddProfile, None);
        form.fields[0].value = path.to_str().unwrap().into();
        assert!(build(&form).is_err(), "derived profile name has spaces");
        form.fields[2].value = "equipe_a".into();
        form.fields[3].value = "sim".into();
        let args = build(&form).unwrap().args;
        assert_eq!(args.last().unwrap(), path.as_os_str());
        assert!(args.contains(&"--no-compile".into()));
        form.fields[1].value = "../escape".into();
        assert!(build(&form).is_err());
    }

    #[test]
    fn import_requires_valid_dataset_and_export_never_overwrites_a_file() {
        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join("dataset.json");
        let mut import = form(Action::Import, None);
        import.fields[0].value = input.to_str().unwrap().into();
        assert!(build(&import).is_err());
        std::fs::write(&input, br#"{"hello": "world"}"#).unwrap();
        assert!(build(&import).is_err());
        std::fs::write(&input, br#"{"sessions":[],"usage":[],"turns":[]}"#).unwrap();
        assert!(build(&import).is_ok());
        let mut report = form(Action::Report, None);
        report.fields[3].value = input.to_str().unwrap().into();
        assert!(build(&report).is_err());
        let output = directory.path().join("relatório novo.json");
        report.fields[3].value = output.to_str().unwrap().into();
        let invocation = build(&report).unwrap();
        assert_eq!(invocation.export_path.as_deref(), Some(output.as_path()));
        assert!(!output.exists(), "validation must not write files");
    }

    #[test]
    fn price_requires_explicit_rates_and_timezone() {
        let mut form = form(Action::Price, None);
        assert!(build(&form).is_err());
        for (index, value) in [
            (0, "provider"),
            (1, "model"),
            (4, "1,5"),
            (5, "0"),
            (6, "0"),
            (7, "2"),
            (8, "Fonte informada pelo usuário"),
        ] {
            form.fields[index].value = value.into();
        }
        let args = strings(build(&form).unwrap());
        assert!(args.contains(&"--input=1.5".into()));
        form.fields[3].value = "2026-09-12".into();
        assert!(build(&form).is_err());
        form.fields[3].value = "2026-09-12T00:00:00-03:00".into();
        form.fields[4].value = "-1".into();
        assert!(build(&form).is_err());
    }

    #[test]
    fn interaction_and_no_argument_forms_are_explicit() {
        assert!(build(&form(Action::Setup, None)).unwrap().interactive);
        assert!(build(&form(Action::Widget, None)).unwrap().interactive);
        assert!(!build(&form(Action::Sync, None)).unwrap().interactive);
        assert_eq!(strings(build(&form(Action::Sync, None)).unwrap()), ["sync"]);
        let mut malformed = form(Action::Run, None);
        malformed.fields.clear();
        assert!(build(&malformed).is_err());
    }

    #[test]
    fn removed_projects_remain_filterable_and_long_benchmarks_fail_before_launch() {
        let directory = tempfile::tempdir().unwrap();
        let removed_project = directory.path().join("removed-worktree");
        let mut report = form(Action::Report, None);
        report.fields[1].value = removed_project.to_str().unwrap().into();
        assert!(
            strings(build(&report).unwrap())
                .contains(&format!("--project={}", removed_project.display()))
        );
        let mut run = form(Action::Run, None);
        run.fields[0].value = "Pedido".into();
        run.fields[2].value = "x".repeat(301);
        assert!(build(&run).is_err());
    }
}
