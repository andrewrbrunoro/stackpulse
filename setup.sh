#!/bin/sh
# StackPulse — local installation for macOS and Linux, without sudo.
set -eu

usage() {
    cat <<'HELP'
StackPulse · instalação local

Uso: ./setup.sh [opções]

  --prefix DIRETÓRIO   Instala em DIRETÓRIO/bin (padrão: ~/.local)
  --no-path            Não altera a configuração do shell
  --shell-rc ARQUIVO   Arquivo de inicialização bash/zsh para preparar o PATH
  --with-memory       Fixa AI-Memory como selecionado
  --without-memory    Pula AI-Memory; não altera a opção AI-UsageBar
  --with-usagebar     Fixa AI-UsageBar como selecionado em sistema compatível
  --without-usagebar  Pula AI-UsageBar; não altera a opção AI-Memory
  --force              Permite substituir um stackpulse não gerenciado
  -h, --help           Mostra esta ajuda

Requer Cargo/Rust. A versão do Rust é definida em rust-toolchain.toml.
Compila a aplicação, instala o comando stackpulse e preserva perfis e histórico.
Use stackpulse update ou /update no chat para atualizar. Nenhum provider é chamado pelo instalador.
Uma única página reúne os plugins; use Espaço para marcar e Enter para continuar.
AI-Memory vem selecionado por padrão. Flags explícitas ficam fixas na seleção.
Sem terminal interativo, o padrão instala AI-Memory; use --without-memory para pular.
AI-UsageBar é oferecido somente em sistemas compatíveis e vem desmarcado por padrão.
Sem terminal interativo, AI-UsageBar não é instalado sem --with-usagebar.
AI-UsageBar é usado pelo CLI e TUI; o instalador não ativa autostart ou menu bar.
HELP
}
fail() { printf 'Erro: %s\n' "$*" >&2; exit 1; }
info() { printf '%s\n' "$*"; }

# Keep the summary in scrollback and commands directly copyable. No cursor
# movement, prompt, or extra dependency is needed to finish an installation.
finish_installation() {
    STACKPULSE_ACCENT=
    STACKPULSE_BOLD=
    STACKPULSE_MUTED=
    STACKPULSE_GREEN=
    STACKPULSE_RESET=
    if [ -t 1 ] && [ -n "${TERM:-}" ] && [ "$TERM" != dumb ] && [ "${NO_COLOR+x}" != x ]; then
        STACKPULSE_ACCENT=$(printf '\033[1;36m')
        STACKPULSE_BOLD=$(printf '\033[1m')
        STACKPULSE_MUTED=$(printf '\033[90m')
        STACKPULSE_GREEN=$(printf '\033[1;32m')
        STACKPULSE_RESET=$(printf '\033[0m')
    fi
    STACKPULSE_COLUMNS=${COLUMNS:-80}
    if [ -t 1 ] && command -v stty >/dev/null 2>&1; then
        set -- $(stty size <&1 2>/dev/null || :)
        [ "$#" -ne 2 ] || STACKPULSE_COLUMNS=$2
    fi
    case "$STACKPULSE_COLUMNS" in
        ''|*[!0-9]*) STACKPULSE_COLUMNS=80 ;;
    esac
    # Leading zeros and oversized environment values must not reach arithmetic.
    case "$STACKPULSE_COLUMNS" in
        [1-9]|[1-9][0-9]|[1-9][0-9][0-9]) ;;
        *) STACKPULSE_COLUMNS=80 ;;
    esac

    printf '\n  %s╭─ STACKPULSE%s  %s%s%s\n' "$STACKPULSE_ACCENT" "$STACKPULSE_RESET" "$STACKPULSE_MUTED" "${STACKPULSE_VERSION#stackpulse }" "$STACKPULSE_RESET"
    if [ "$STACKPULSE_MEMORY_EXIT" -ne 0 ] || [ "$STACKPULSE_USAGEBAR_EXIT" -ne 0 ]; then
        printf '  %s│%s  %s✓ StackPulse instalado%s\n' "$STACKPULSE_ACCENT" "$STACKPULSE_RESET" "$STACKPULSE_GREEN" "$STACKPULSE_RESET"
    else
        printf '  %s│%s  %s✓ Instalação concluída%s\n' "$STACKPULSE_ACCENT" "$STACKPULSE_RESET" "$STACKPULSE_GREEN" "$STACKPULSE_RESET"
    fi
    printf '  %s╰─%s %sTokens, tempo e entrega.%s\n' "$STACKPULSE_ACCENT" "$STACKPULSE_RESET" "$STACKPULSE_MUTED" "$STACKPULSE_RESET"

    printf '\n  %sEXECUTÁVEL%s' "$STACKPULSE_MUTED" "$STACKPULSE_RESET"
    if [ "$STACKPULSE_COLUMNS" -lt 60 ]; then
        printf '\n  %s\n' "$STACKPULSE_DEST"
        printf '  %sPATH%s\n ' "$STACKPULSE_MUTED" "$STACKPULSE_RESET"
    else
        printf '  %s\n' "$STACKPULSE_DEST"
        printf '  %sPATH%s       ' "$STACKPULSE_MUTED" "$STACKPULSE_RESET"
    fi
    if [ "$STACKPULSE_PATH_PRESENT" -eq 1 ]; then
        printf ' %sDisponível neste terminal%s\n' "$STACKPULSE_GREEN" "$STACKPULSE_RESET"
    elif [ "$STACKPULSE_NO_PATH" -eq 0 ] && [ -n "$STACKPULSE_RC" ]; then
        printf ' Preparado para novos terminais\n'
        printf '  %s%s%s\n' "$STACKPULSE_MUTED" "$STACKPULSE_RC" "$STACKPULSE_RESET"
    else
        printf ' Ativação manual\n'
    fi

    printf '\n  %sMEMÓRIA OPCIONAL%s\n' "$STACKPULSE_MUTED" "$STACKPULSE_RESET"
    case "$STACKPULSE_MEMORY_STATUS" in
        available)
            printf '  %s✓ AI-Memory disponível%s\n' "$STACKPULSE_GREEN" "$STACKPULSE_RESET"
            printf '  Conexão com os CLIs é uma etapa separada.\n'
            ;;
        skipped)
            printf '  AI-Memory · não selecionado\n'
            ;;
        failed)
            printf '  AI-Memory · instalação falhou\n'
            printf '  StackPulse foi instalado e pode ser usado.\n'
            printf '  Consulte o erro acima para tentar novamente.\n'
            ;;
    esac
    printf '  Criado por Fabio Akita (AkitaOnRails)\n'
    printf '  https://github.com/akitaonrails/ai-memory\n'
    printf '  https://akitaonrails.com\n'

    if [ "$STACKPULSE_USAGEBAR_STATUS" != hidden ]; then
        printf '\n  %sCONSUMO OPCIONAL%s\n' "$STACKPULSE_MUTED" "$STACKPULSE_RESET"
        case "$STACKPULSE_USAGEBAR_STATUS" in
            available)
                printf '  %s✓ AI-UsageBar disponível%s\n' "$STACKPULSE_GREEN" "$STACKPULSE_RESET"
                printf '  Acesso pelo CLI e TUI, sem autostart ou menu bar.\n'
                ;;
            skipped)
                printf '  AI-UsageBar · não selecionado\n'
                ;;
            unsupported)
                printf '  AI-UsageBar · sistema incompatível\n'
                printf '  Nenhum download de AI-UsageBar foi iniciado.\n'
                ;;
            failed)
                printf '  AI-UsageBar · instalação falhou\n'
                printf '  Consulte o erro acima para tentar novamente.\n'
                ;;
        esac
        if [ "$STACKPULSE_USAGEBAR_EXIT" -ne 0 ]; then
            printf '  StackPulse foi instalado e pode ser usado.\n'
        fi
        printf '  Criado por Fabio Akita (AkitaOnRails)\n'
        printf '  https://github.com/akitaonrails/ai-usagebar\n'
        printf '  https://akitaonrails.com\n'
    fi

    printf '\n  %sCOMECE AQUI%s\n' "$STACKPULSE_BOLD" "$STACKPULSE_RESET"
    if [ "$STACKPULSE_PATH_PRESENT" -eq 0 ]; then
        printf '  %s1%s  Ative nesta sessão\n' "$STACKPULSE_ACCENT" "$STACKPULSE_RESET"
        printf '     export PATH=%s:"$PATH"\n' "$STACKPULSE_QUOTED_BIN"
        printf '  %s2%s  Na pasta do projeto, execute\n' "$STACKPULSE_ACCENT" "$STACKPULSE_RESET"
    else
        printf '  Na pasta do projeto, execute\n'
    fi
    printf '     %sstackpulse%s\n' "$STACKPULSE_ACCENT" "$STACKPULSE_RESET"
    printf '  Autorize a pasta, escolha um perfil\n'
    printf '  e envie seu primeiro pedido.\n'

    printf '\n  %sATALHOS%s\n' "$STACKPULSE_MUTED" "$STACKPULSE_RESET"
    if [ "$STACKPULSE_COLUMNS" -lt 60 ]; then
        printf '  %sstackpulse setup%s\n    Configurar os CLIs\n' "$STACKPULSE_BOLD" "$STACKPULSE_RESET"
        printf '  %sstackpulse widget%s\n    Abrir widget compacto\n' "$STACKPULSE_BOLD" "$STACKPULSE_RESET"
    else
        printf '  %sstackpulse setup%s    %sConfigurar os CLIs%s\n' "$STACKPULSE_BOLD" "$STACKPULSE_RESET" "$STACKPULSE_MUTED" "$STACKPULSE_RESET"
        printf '  %sstackpulse widget%s   %sAbrir widget compacto%s\n' "$STACKPULSE_BOLD" "$STACKPULSE_RESET" "$STACKPULSE_MUTED" "$STACKPULSE_RESET"
    fi
    printf '\n  %sPerfis e histórico preservados.%s\n' "$STACKPULSE_MUTED" "$STACKPULSE_RESET"
}

STACKPULSE_PREFIX=${HOME:?HOME precisa estar definido}/.local
STACKPULSE_NO_PATH=0
STACKPULSE_FORCE=0
STACKPULSE_RC=
STACKPULSE_EXPLICIT_RC=0
STACKPULSE_STAGE=
STACKPULSE_MANIFEST_STAGE=
STACKPULSE_SELECTION_DIR=
STACKPULSE_MEMORY_MODE=select
STACKPULSE_MEMORY_STATUS=skipped
STACKPULSE_MEMORY_EXIT=0
STACKPULSE_USAGEBAR_MODE=select
STACKPULSE_USAGEBAR_STATUS=hidden
STACKPULSE_USAGEBAR_SUPPORTED=0
STACKPULSE_USAGEBAR_EXIT=0
while [ "$#" -gt 0 ]; do
    case "$1" in
        --prefix)
            [ "$#" -ge 2 ] && [ -n "$2" ] || fail '--prefix precisa de um diretório.'
            STACKPULSE_PREFIX=$2; shift 2 ;;
        --shell-rc)
            [ "$#" -ge 2 ] && [ -n "$2" ] || fail '--shell-rc precisa de um arquivo.'
            STACKPULSE_RC=$2; STACKPULSE_EXPLICIT_RC=1; shift 2 ;;
        --no-path) STACKPULSE_NO_PATH=1; shift ;;
        --with-memory)
            [ "$STACKPULSE_MEMORY_MODE" != without ] || fail '--with-memory e --without-memory não podem ser combinados.'
            STACKPULSE_MEMORY_MODE=with; shift ;;
        --without-memory)
            [ "$STACKPULSE_MEMORY_MODE" != with ] || fail '--with-memory e --without-memory não podem ser combinados.'
            STACKPULSE_MEMORY_MODE=without; shift ;;
        --with-usagebar)
            [ "$STACKPULSE_USAGEBAR_MODE" != without ] || fail '--with-usagebar e --without-usagebar não podem ser combinados.'
            STACKPULSE_USAGEBAR_MODE=with; shift ;;
        --without-usagebar)
            [ "$STACKPULSE_USAGEBAR_MODE" != with ] || fail '--with-usagebar e --without-usagebar não podem ser combinados.'
            STACKPULSE_USAGEBAR_MODE=without; shift ;;
        --force) STACKPULSE_FORCE=1; shift ;;
        -h|--help) usage; exit 0 ;;
        *) fail "Opção desconhecida: $1. Use --help." ;;
    esac
done
[ "$STACKPULSE_NO_PATH" -eq 0 ] || [ "$STACKPULSE_EXPLICIT_RC" -eq 0 ] || fail '--no-path e --shell-rc não podem ser combinados.'

case $(uname -s) in
    Darwin|Linux) ;;
    *) fail 'Este instalador suporta macOS e Linux.' ;;
esac
case "$STACKPULSE_PREFIX" in
    *:*) fail 'O diretório de instalação não pode conter dois-pontos (separador do PATH).' ;;
esac
# Newlines cannot be represented as one managed shell startup line.
case "$STACKPULSE_PREFIX$STACKPULSE_RC" in
    *'
'*|*''*) fail 'Use caminhos sem quebras de linha.' ;;
esac

STACKPULSE_SOURCE=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd -P)
case "$STACKPULSE_SOURCE" in
    *'
'*|*''*) fail 'Use uma pasta de fontes sem quebras de linha.' ;;
esac
[ -f "$STACKPULSE_SOURCE/Cargo.toml" ] && [ -f "$STACKPULSE_SOURCE/rust-toolchain.toml" ] || fail 'Mantenha setup.sh na pasta do projeto, junto de Cargo.toml.'
case "$STACKPULSE_PREFIX" in
    /*) ;;
    *) STACKPULSE_PREFIX=$(pwd -P)/$STACKPULSE_PREFIX ;;
esac
case "$STACKPULSE_RC" in
    ''|/*) ;;
    *) STACKPULSE_RC=$(pwd -P)/$STACKPULSE_RC ;;
esac
STACKPULSE_BIN=$STACKPULSE_PREFIX/bin
STACKPULSE_DEST=$STACKPULSE_BIN/stackpulse
STACKPULSE_SHARE=$STACKPULSE_PREFIX/share/stackpulse
STACKPULSE_MANIFEST=$STACKPULSE_SHARE/install.manifest

# Respect an existing Cargo installation, including shells missing ~/.cargo/bin.
STACKPULSE_CARGO=$(command -v cargo 2>/dev/null || :)
if [ -z "$STACKPULSE_CARGO" ]; then
    STACKPULSE_CARGO=${CARGO_HOME:-$HOME/.cargo}/bin/cargo
fi
[ -x "$STACKPULSE_CARGO" ] || fail 'Cargo não encontrado. Instale Rust com rustup, reabra o terminal e execute ./setup.sh novamente.'
case "$STACKPULSE_CARGO" in
    /*) ;;
    *) STACKPULSE_CARGO=$(pwd -P)/$STACKPULSE_CARGO ;;
esac

# Never execute an existing binary to establish ownership. cksum only reads it.
managed_install() {
    [ -f "$STACKPULSE_DEST" ] && [ ! -L "$STACKPULSE_DEST" ] && [ -f "$STACKPULSE_MANIFEST" ] || return 1
    [ "$(sed -n '1p' "$STACKPULSE_MANIFEST")" = 'stackpulse-installer-v1' ] || return 1
    STACKPULSE_SAVED_SUM=$(sed -n '2p' "$STACKPULSE_MANIFEST")
    [ -n "$STACKPULSE_SAVED_SUM" ] && [ "$STACKPULSE_SAVED_SUM" = "$(cksum < "$STACKPULSE_DEST")" ]
}
if [ -e "$STACKPULSE_DEST" ] || [ -L "$STACKPULSE_DEST" ]; then
    [ ! -d "$STACKPULSE_DEST" ] || fail "$STACKPULSE_DEST é um diretório. Escolha outro --prefix."
    if [ "$STACKPULSE_FORCE" -eq 0 ] && ! managed_install; then
        fail "Já existe um stackpulse não gerenciado ou alterado em $STACKPULSE_DEST. Use outro --prefix ou --force para substituí-lo."
    fi
fi

# Quote literal paths for a POSIX-compatible shell, including apostrophes and $().
shell_quote() {
    printf "'"
    printf '%s' "$1" | sed "s/'/'\\\\''/g"
    printf "'"
}
STACKPULSE_QUOTED_BIN=$(shell_quote "$STACKPULSE_BIN")
STACKPULSE_PATH_LINE=$(printf 'case ":$PATH:" in *:%s:*) ;; *) export PATH=%s:"$PATH" ;; esac' "$STACKPULSE_QUOTED_BIN" "$STACKPULSE_QUOTED_BIN")
STACKPULSE_PATH_PRESENT=0
case ":${PATH-}:" in
    *:"$STACKPULSE_BIN":*) STACKPULSE_PATH_PRESENT=1 ;;
esac

if [ "$STACKPULSE_NO_PATH" -eq 0 ] && [ "$STACKPULSE_PATH_PRESENT" -eq 0 ]; then
    if [ "$STACKPULSE_EXPLICIT_RC" -eq 0 ]; then
        STACKPULSE_SHELL=${SHELL:-}
        case ${STACKPULSE_SHELL##*/} in
            zsh) STACKPULSE_RC=${ZDOTDIR:-$HOME}/.zshrc ;;
            bash)
                case $(uname -s) in
                    Darwin) STACKPULSE_RC=$HOME/.bash_profile ;;
                    *) STACKPULSE_RC=$HOME/.bashrc ;;
                esac ;;
            sh|dash) STACKPULSE_RC=$HOME/.profile ;;
            *) STACKPULSE_RC= ;;
        esac
    fi
    if [ -n "$STACKPULSE_RC" ] && [ -e "$STACKPULSE_RC" ]; then
        [ -f "$STACKPULSE_RC" ] && [ -w "$STACKPULSE_RC" ] || fail "Sem acesso para atualizar $STACKPULSE_RC. Use --no-path para configurar o PATH manualmente."
    fi
fi

cleanup() {
    [ -z "$STACKPULSE_STAGE" ] || rm -f -- "$STACKPULSE_STAGE"
    [ -z "$STACKPULSE_MANIFEST_STAGE" ] || rm -f -- "$STACKPULSE_MANIFEST_STAGE"
    [ -z "$STACKPULSE_SELECTION_DIR" ] || rm -rf -- "$STACKPULSE_SELECTION_DIR"
}
trap cleanup 0
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP

info 'StackPulse · compilando a versão de produção…'
# The working directory selects the repository's pinned rustup toolchain.
# An explicit target directory avoids guessing Cargo's output location.
if ! (
    cd -- "$STACKPULSE_SOURCE"
    "$STACKPULSE_CARGO" build --release --locked --bin ai-token-timeline --target-dir "$STACKPULSE_SOURCE/target"
); then
    fail 'A compilação falhou. A instalação anterior foi preservada.'
fi
STACKPULSE_BUILD=$STACKPULSE_SOURCE/target/release/ai-token-timeline
[ -f "$STACKPULSE_BUILD" ] && [ -x "$STACKPULSE_BUILD" ] || fail 'Cargo terminou sem gerar o executável esperado.'

mkdir -p -- "$STACKPULSE_BIN" "$STACKPULSE_SHARE"
STACKPULSE_STAGE=$(mktemp "$STACKPULSE_BIN/.stackpulse.XXXXXX")
cp -- "$STACKPULSE_BUILD" "$STACKPULSE_STAGE"
chmod 755 "$STACKPULSE_STAGE"
STACKPULSE_VERSION=$("$STACKPULSE_STAGE" --version) || fail 'O executável compilado não passou na verificação de versão.'
case "$STACKPULSE_VERSION" in
    'stackpulse '*) ;;
    *) fail "Executável inesperado: $STACKPULSE_VERSION" ;;
esac
STACKPULSE_MANIFEST_STAGE=$(mktemp "$STACKPULSE_SHARE/.install.XXXXXX")
{
    printf '%s\n' 'stackpulse-installer-v1'
    cksum < "$STACKPULSE_STAGE"
    printf '%s\n' "$STACKPULSE_SOURCE"
} > "$STACKPULSE_MANIFEST_STAGE"
chmod 644 "$STACKPULSE_MANIFEST_STAGE"
# Rename avoids partial binaries and replaces symlinks without following them.
mv -f -- "$STACKPULSE_STAGE" "$STACKPULSE_DEST"
STACKPULSE_STAGE=
mv -f -- "$STACKPULSE_MANIFEST_STAGE" "$STACKPULSE_MANIFEST"
STACKPULSE_MANIFEST_STAGE=

if [ "$STACKPULSE_NO_PATH" -eq 0 ] && [ "$STACKPULSE_PATH_PRESENT" -eq 0 ] && [ -n "$STACKPULSE_RC" ]; then
    mkdir -p -- "$(dirname -- "$STACKPULSE_RC")"
    # Append only our missing line; preserve user content and dotfile symlinks.
    if [ ! -f "$STACKPULSE_RC" ] || ! grep -Fqx -- "$STACKPULSE_PATH_LINE" "$STACKPULSE_RC"; then
        printf '\n# StackPulse: comando no PATH\n%s\n' "$STACKPULSE_PATH_LINE" >> "$STACKPULSE_RC"
    fi
fi

# The compatibility probe is read-only and never downloads dependencies.
# Keep the optional integrations independent, including after a partial failure.
if [ "$STACKPULSE_USAGEBAR_MODE" = without ]; then
    STACKPULSE_USAGEBAR_STATUS=skipped
elif "$STACKPULSE_DEST" --allow-workspace "$(pwd -P)" usagebar supported; then
    STACKPULSE_USAGEBAR_SUPPORTED=1
    STACKPULSE_USAGEBAR_STATUS=skipped
else
    STACKPULSE_USAGEBAR_PROBE_EXIT=$?
    if [ "$STACKPULSE_USAGEBAR_PROBE_EXIT" -ne 1 ]; then
        STACKPULSE_USAGEBAR_EXIT=$STACKPULSE_USAGEBAR_PROBE_EXIT
        [ "$STACKPULSE_USAGEBAR_MODE" != with ] || STACKPULSE_USAGEBAR_STATUS=failed
        printf 'Erro: não foi possível verificar a compatibilidade de AI-UsageBar (código %s). Nenhum download de AI-UsageBar foi iniciado.\n' "$STACKPULSE_USAGEBAR_PROBE_EXIT" >&2
    elif [ "$STACKPULSE_USAGEBAR_MODE" = with ]; then
        STACKPULSE_USAGEBAR_STATUS=unsupported
        STACKPULSE_USAGEBAR_EXIT=1
        printf 'Erro: AI-UsageBar não é compatível com este sistema. Nenhum download de AI-UsageBar foi iniciado.\n' >&2
    fi
fi

# Unsupported integrations have no editable selection. Keep an explicit
# compatibility error above while allowing the other plugin to be selected.
STACKPULSE_SELECTED_MEMORY=$STACKPULSE_MEMORY_MODE
STACKPULSE_SELECTED_USAGEBAR=$STACKPULSE_USAGEBAR_MODE
[ "$STACKPULSE_USAGEBAR_SUPPORTED" -eq 1 ] || STACKPULSE_SELECTED_USAGEBAR=without
if [ "$STACKPULSE_SELECTED_MEMORY" = select ] || [ "$STACKPULSE_SELECTED_USAGEBAR" = select ]; then
    STACKPULSE_SELECTION_DIR=$(mktemp -d "${TMPDIR:-/tmp}/stackpulse-plugins.XXXXXX")
    case "$STACKPULSE_SELECTION_DIR" in
        /*) ;;
        *) STACKPULSE_SELECTION_DIR=$(pwd -P)/$STACKPULSE_SELECTION_DIR ;;
    esac
    STACKPULSE_SELECTION_RESULT=$STACKPULSE_SELECTION_DIR/selection
    # Run directly so stdin/stdout remain attached to the user's terminal.
    if "$STACKPULSE_DEST" --allow-workspace "$(pwd -P)" plugins select \
        --memory "$STACKPULSE_SELECTED_MEMORY" --usagebar "$STACKPULSE_SELECTED_USAGEBAR" \
        --result-file "$STACKPULSE_SELECTION_RESULT"; then
        :
    else
        STACKPULSE_SELECTION_EXIT=$?
        printf 'Erro: seleção de plugins encerrada sem confirmação (código %s). Nenhum plugin foi instalado. StackPulse permanece disponível.\n' "$STACKPULSE_SELECTION_EXIT" >&2
        exit "$STACKPULSE_SELECTION_EXIT"
    fi
    [ -f "$STACKPULSE_SELECTION_RESULT" ] && [ ! -L "$STACKPULSE_SELECTION_RESULT" ] || fail 'Seleção de plugins sem arquivo de resultado válido. Nenhum plugin foi instalado.'
    # The result is data, never shell code. Require exactly two complete lines.
    STACKPULSE_SELECTION_MEMORY=
    STACKPULSE_SELECTION_USAGEBAR=
    STACKPULSE_SELECTION_EXTRA=
    exec 3< "$STACKPULSE_SELECTION_RESULT"
    IFS= read -r STACKPULSE_SELECTION_MEMORY <&3 || fail 'Resultado de seleção de plugins inválido. Nenhum plugin foi instalado.'
    IFS= read -r STACKPULSE_SELECTION_USAGEBAR <&3 || fail 'Resultado de seleção de plugins inválido. Nenhum plugin foi instalado.'
    if IFS= read -r STACKPULSE_SELECTION_EXTRA <&3 || [ -n "$STACKPULSE_SELECTION_EXTRA" ]; then
        fail 'Resultado de seleção de plugins inválido. Nenhum plugin foi instalado.'
    fi
    exec 3<&-
    case "$STACKPULSE_SELECTION_MEMORY" in
        memory=with|memory=without) ;;
        *) fail 'Resultado de seleção de AI-Memory inválido. Nenhum plugin foi instalado.' ;;
    esac
    case "$STACKPULSE_SELECTION_USAGEBAR" in
        usagebar=with|usagebar=without) ;;
        *) fail 'Resultado de seleção de AI-UsageBar inválido. Nenhum plugin foi instalado.' ;;
    esac
    printf '%s\n%s\n' "$STACKPULSE_SELECTION_MEMORY" "$STACKPULSE_SELECTION_USAGEBAR" > "$STACKPULSE_SELECTION_DIR/expected"
    cmp -s "$STACKPULSE_SELECTION_RESULT" "$STACKPULSE_SELECTION_DIR/expected" || fail 'Resultado de seleção de plugins contém dados inesperados. Nenhum plugin foi instalado.'
    if [ "$STACKPULSE_SELECTED_MEMORY" != select ] && [ "$STACKPULSE_SELECTION_MEMORY" != "memory=$STACKPULSE_SELECTED_MEMORY" ]; then
        fail 'A seleção alterou uma opção fixa de AI-Memory. Nenhum plugin foi instalado.'
    fi
    if [ "$STACKPULSE_SELECTED_USAGEBAR" != select ] && [ "$STACKPULSE_SELECTION_USAGEBAR" != "usagebar=$STACKPULSE_SELECTED_USAGEBAR" ]; then
        fail 'A seleção alterou uma opção fixa de AI-UsageBar. Nenhum plugin foi instalado.'
    fi
    STACKPULSE_SELECTED_MEMORY=${STACKPULSE_SELECTION_MEMORY#memory=}
    STACKPULSE_SELECTED_USAGEBAR=${STACKPULSE_SELECTION_USAGEBAR#usagebar=}
    rm -rf -- "$STACKPULSE_SELECTION_DIR"
    STACKPULSE_SELECTION_DIR=
fi

if [ "$STACKPULSE_SELECTED_MEMORY" = with ]; then
    # Selection was already confirmed for both integrations. Installation never
    # opens another checkbox; each plugin keeps its own diagnostics and status.
    set -- --allow-workspace "$(pwd -P)" memory install --prefix "$STACKPULSE_PREFIX"
    if "$STACKPULSE_DEST" "$@"; then
        STACKPULSE_MEMORY_STATUS=available
    else
        STACKPULSE_MEMORY_EXIT=$?
        if [ "$STACKPULSE_MEMORY_EXIT" -eq 10 ]; then
            STACKPULSE_MEMORY_STATUS=skipped
            STACKPULSE_MEMORY_EXIT=0
        else
            STACKPULSE_MEMORY_STATUS=failed
        fi
    fi
fi

if [ "$STACKPULSE_USAGEBAR_SUPPORTED" -eq 1 ] && [ "$STACKPULSE_SELECTED_USAGEBAR" = with ]; then
    set -- --allow-workspace "$(pwd -P)" usagebar install --prefix "$STACKPULSE_PREFIX"
    if "$STACKPULSE_DEST" "$@"; then
        STACKPULSE_USAGEBAR_STATUS=available
    else
        STACKPULSE_USAGEBAR_EXIT=$?
        if [ "$STACKPULSE_USAGEBAR_EXIT" -eq 10 ]; then
            STACKPULSE_USAGEBAR_STATUS=skipped
            STACKPULSE_USAGEBAR_EXIT=0
        else
            STACKPULSE_USAGEBAR_STATUS=failed
        fi
    fi
fi

finish_installation
[ "$STACKPULSE_MEMORY_EXIT" -eq 0 ] || exit "$STACKPULSE_MEMORY_EXIT"
exit "$STACKPULSE_USAGEBAR_EXIT"
