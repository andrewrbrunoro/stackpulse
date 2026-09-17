//! Local browser previews for assistant responses containing Mermaid diagrams.

use std::io::Write;
use std::path::PathBuf;

use anyhow::{Context, Result};

/// Detect a complete Mermaid fence, ignoring fence-like text inside other fences.
/// Like Markdown's top-level fenced blocks, fences may have up to three spaces.
pub(crate) fn has_mermaid(markdown: &str) -> bool {
    let mut open: Option<(u8, usize, bool)> = None;
    for line in markdown.lines() {
        let indent = line.bytes().take_while(|byte| *byte == b' ').count();
        if indent > 3 {
            continue;
        }
        let line = &line[indent..];
        let Some(marker @ (b'`' | b'~')) = line.bytes().next() else {
            continue;
        };
        let length = line.bytes().take_while(|byte| *byte == marker).count();
        let suffix = &line[length..];
        if let Some((opening_marker, opening_length, mermaid)) = open {
            if marker == opening_marker && length >= opening_length && suffix.trim().is_empty() {
                if mermaid {
                    return true;
                }
                open = None;
            }
        } else if length >= 3 && (marker != b'`' || !suffix.contains('`')) {
            let mermaid = suffix
                .split_whitespace()
                .next()
                .is_some_and(|language| language.eq_ignore_ascii_case("mermaid"));
            open = Some((marker, length, mermaid));
        }
    }
    false
}

/// Keep the temporary HTML alive after returning, so an asynchronously opened
/// browser can read it. The OS may remove it during normal temporary-file cleanup.
pub(crate) fn write_preview(markdown: &str) -> Result<PathBuf> {
    let mut file = tempfile::Builder::new()
        .prefix("stackpulse-preview-")
        .suffix(".html")
        .tempfile()
        .context("Não foi possível criar a prévia da resposta")?;
    file.write_all(preview_html(markdown).as_bytes())
        .context("Não foi possível gravar a prévia da resposta")?;
    let (_, path) = file
        .keep()
        .context("Não foi possível preservar a prévia da resposta")?;
    Ok(path)
}

fn escape_html(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn preview_html(markdown: &str) -> String {
    let nonce = uuid::Uuid::new_v4().simple().to_string();
    // The response is inserted only as escaped text, never into JavaScript or
    // raw HTML. textContent retrieves the original Markdown after page loading.
    let mut html = TEMPLATE.replace("__NONCE__", &nonce);
    html = html.replace("__MARKDOWN__", &escape_html(markdown));
    html
}

const TEMPLATE: &str = r##"<!doctype html>
<html lang="pt-BR">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <meta name="referrer" content="no-referrer">
  <meta http-equiv="Content-Security-Policy" content="default-src 'none'; script-src 'nonce-__NONCE__' https://cdn.jsdelivr.net; style-src 'unsafe-inline'; img-src data:; font-src data:; connect-src 'none'; base-uri 'none'; form-action 'none'">
  <title>StackPulse · Resposta</title>
  <style>
    :root { color-scheme: light; font: 16px/1.6 system-ui, sans-serif; color: #182536; background: #f4f6fa; }
    body { margin: 0 auto; padding: 28px 24px 60px; max-width: 1080px; }
    header { color: #4e6078; margin-bottom: 20px; }
    header strong { color: #182536; }
    #status { font-size: 14px; }
    article, details { padding: 24px; margin: 16px 0; background: white; border: 1px solid #dbe2ec; border-radius: 12px; overflow-wrap: anywhere; }
    article:empty { display: none; }
    h1, h2, h3 { line-height: 1.25; }
    a { color: #245bc0; }
    pre { overflow-x: auto; padding: 16px; background: #f2f5f9; border-radius: 8px; }
    code { font-family: ui-monospace, monospace; font-size: .9em; }
    blockquote { margin-left: 0; padding-left: 18px; border-left: 3px solid #a6bbdd; color: #4e6078; }
    table { border-collapse: collapse; display: block; overflow-x: auto; }
    th, td { border: 1px solid #dbe2ec; padding: 8px 12px; }
    th { background: #f2f5f9; }
    img { max-width: 100%; }
    .diagram { overflow-x: auto; text-align: center; padding: 16px 0; }
    .diagram svg { max-width: 100%; height: auto; }
    .render-error { color: #a33324; font-size: 14px; }
    #source { white-space: pre-wrap; }
    summary { cursor: pointer; }
  </style>
</head>
<body>
  <header><strong>StackPulse</strong> · Resposta renderizada
    <div id="status" role="status">Carregando a renderização. É necessário acesso à internet para carregar as bibliotecas.</div>
  </header>
  <article id="content"></article>
  <details id="original" open><summary>Markdown original</summary><pre id="source">__MARKDOWN__</pre></details>
  <noscript>Ative JavaScript para renderizar o Markdown e os diagramas.</noscript>
  <script nonce="__NONCE__">
    (async () => {
      const status = document.getElementById('status');
      const content = document.getElementById('content');
      const source = document.getElementById('source').textContent;
      const original = document.getElementById('original');
      const nonce = document.currentScript.nonce;
      const loadScript = (url) => new Promise((resolve, reject) => {
        const script = document.createElement('script');
        const timeout = setTimeout(() => reject(new Error('Tempo de carregamento excedido')), 15000);
        script.src = url;
        script.nonce = nonce;
        script.onload = () => { clearTimeout(timeout); resolve(); };
        script.onerror = () => { clearTimeout(timeout); reject(new Error('Biblioteca indisponível')); };
        document.head.appendChild(script);
      });
      try {
        await Promise.all([
          loadScript('https://cdn.jsdelivr.net/npm/marked@18.0.13/lib/marked.umd.js'),
          loadScript('https://cdn.jsdelivr.net/npm/dompurify@3.4.15/dist/purify.min.js')
        ]);
        content.innerHTML = DOMPurify.sanitize(marked.parse(source), {
          USE_PROFILES: { html: true },
          FORBID_TAGS: ['style'],
          FORBID_ATTR: ['style'],
          SANITIZE_NAMED_PROPS: true
        });
        for (const link of content.querySelectorAll('a')) {
          link.rel = 'noopener noreferrer';
        }
        original.open = false;
        const blocks = [...content.querySelectorAll('pre > code')].filter((code) =>
          [...code.classList].some((name) => name.toLowerCase() === 'language-mermaid'));
        if (!blocks.length) {
          status.textContent = 'Markdown renderizado.';
          return;
        }
        let importTimeout;
        const module = await Promise.race([
          import('https://cdn.jsdelivr.net/npm/mermaid@12.0.0/dist/mermaid.esm.min.mjs'),
          new Promise((_, reject) => {
            importTimeout = setTimeout(() => reject(new Error('Tempo de carregamento excedido')), 15000);
          })
        ]).finally(() => clearTimeout(importTimeout));
        const mermaid = module.default;
        mermaid.initialize({
          startOnLoad: false,
          securityLevel: 'strict',
          theme: 'default',
          htmlLabels: false,
          flowchart: { htmlLabels: false }
        });
        let failures = 0;
        for (const [index, code] of blocks.entries()) {
          const pre = code.parentElement;
          const diagram = document.createElement('div');
          diagram.className = 'diagram';
          pre.before(diagram);
          try {
            const { svg } = await mermaid.render('stackpulse-diagram-' + index, code.textContent, diagram);
            diagram.innerHTML = DOMPurify.sanitize(svg, {
              USE_PROFILES: { svg: true, svgFilters: true }
            });
            pre.remove();
          } catch (error) {
            failures += 1;
            diagram.remove();
            const message = document.createElement('p');
            message.className = 'render-error';
            message.textContent = 'Não foi possível renderizar este diagrama. O código original está abaixo.';
            pre.before(message);
          }
        }
        status.textContent = failures
          ? 'Markdown renderizado; ' + failures + ' diagrama(s) com erro.'
          : 'Markdown e diagramas renderizados.';
      } catch (error) {
        status.textContent = 'Não foi possível carregar a renderização. Verifique a conexão com a internet e recarregue a página. O Markdown original está disponível abaixo.';
        original.open = true;
      }
    })();
  </script>
</body>
</html>
"##;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_complete_mermaid_fences() {
        for markdown in [
            "```mermaid\ngraph TD\nA --> B\n```",
            "Text\n~~~mermaid\ngraph TD\nA --> B\n~~~~\nAfter",
            "   ```` Mermaid title\r\ngraph TD\r\n   `````  \r\n",
            "```rust\nlet x = 1;\n```\n```mermaid\nA --> B\n```",
        ] {
            assert!(has_mermaid(markdown), "{markdown:?}");
        }
    }

    #[test]
    fn ignores_mentions_incomplete_fences_and_other_code_blocks() {
        for markdown in [
            "Use `mermaid` or ```mermaid to draw.",
            "```mermaid\nA --> B",
            "````mermaid\nA --> B\n```",
            "```mermaid\nA --> B\n~~~",
            "```mermaid\nA --> B\n``` trailing text",
            "```mermaid-example\nA --> B\n```",
            "````markdown\n```mermaid\nA --> B\n```\n````",
            "~~~markdown\n```mermaid\nA --> B\n```\n~~~",
            "```text\n~~~mermaid\nA --> B\n~~~\n```",
            "    ```mermaid\n    A --> B\n    ```",
            "\t```mermaid\nA --> B\n```",
            "```mermaid`\nA --> B\n```",
        ] {
            assert!(!has_mermaid(markdown), "{markdown:?}");
        }
    }

    #[test]
    fn source_cannot_escape_its_text_container_or_inject_scripts() {
        let source = "</pre><script>alert('x')</script><img src=x onerror=alert(1)> & \"ação\" __NONCE__ __MARKDOWN__";
        let html = preview_html(source);
        assert!(!html.contains(source));
        assert!(!html.contains("<script>alert"));
        assert!(!html.contains("<img src=x"));
        assert!(html.contains("&lt;/pre&gt;&lt;script&gt;alert(&#39;x&#39;)&lt;/script&gt;"));
        assert!(html.contains("&amp; &quot;ação&quot; __NONCE__ __MARKDOWN__"));
        assert!(html.contains("securityLevel: 'strict'"));
        assert!(html.contains("DOMPurify.sanitize(marked.parse(source)"));
    }

    #[test]
    fn preview_file_survives_return_and_has_an_html_extension() {
        let path = write_preview("# Resposta\n```mermaid\ngraph TD\nA --> B\n```").unwrap();
        assert_eq!(path.extension().unwrap(), "html");
        let html = std::fs::read_to_string(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        assert!(html.starts_with("<!doctype html>"));
        assert!(html.contains("# Resposta\n```mermaid\ngraph TD\nA --&gt; B\n```"));
        assert!(!html.contains("__NONCE__"));
        assert!(!html.contains("__MARKDOWN__"));
    }
}
