#!/bin/sh
# Run with: sh tests/setup.sh. Everything, including Cargo, stays in a temp tree.
set -eu

test_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd -P)
source_dir=$(CDPATH= cd -- "$test_dir/.." && pwd -P)
work=$(mktemp -d "${TMPDIR:-/tmp}/stackpulse-setup-test.XXXXXX")
trap 'rm -rf -- "$work"' EXIT HUP INT TERM

fail() {
    printf 'FAIL: %s\n' "$*" >&2
    if [ -f "$work/output" ]; then
        cat "$work/output" >&2
    fi
    exit 1
}

pass() {
    printf 'ok: %s\n' "$1"
}

[ -f "$source_dir/setup.sh" ] || fail 'setup.sh is missing'
repo="$work/repo with spaces"
tools_dir="$work/tools"
system_tools="$work/system tools"
elsewhere="$work/elsewhere"
mkdir -p "$repo" "$tools_dir" "$system_tools" "$elsewhere"
cp "$source_dir/setup.sh" "$source_dir/Cargo.toml" "$source_dir/Cargo.lock" \
    "$source_dir/rust-toolchain.toml" "$repo/"
# Do not accidentally call a real Cargo installed in /usr/bin on Linux.
for utility in sh uname dirname sed cksum mkdir mktemp cp chmod mv grep cat rm cmp; do
    utility_path=$(command -v "$utility") || fail "missing test prerequisite: $utility"
    ln -s "$utility_path" "$system_tools/$utility"
done

cat > "$tools_dir/cargo" <<'CARGO'
#!/bin/sh
set -eu
printf 'build\n' >> "$STACKPULSE_TEST_LOG"
manifest=
target=
locked=false
release=false
while [ "$#" -gt 0 ]; do
    case "$1" in
        --manifest-path) manifest=$2; shift 2 ;;
        --target-dir) target=$2; shift 2 ;;
        --locked) locked=true; shift ;;
        --release) release=true; shift ;;
        *) shift ;;
    esac
done
[ "$locked" = true ] && [ "$release" = true ] || exit 91
if [ -n "$manifest" ]; then
    [ -f "$manifest" ] && [ -f "$(dirname -- "$manifest")/Cargo.lock" ] || exit 92
else
    [ -f Cargo.toml ] && [ -f Cargo.lock ] || exit 92
fi
[ -n "$target" ] || exit 93
[ "${STACKPULSE_TEST_FAIL_BUILD:-0}" = 0 ] || exit 94
mkdir -p "$target/release"
{
    printf '#!/bin/sh\n'
    printf 'if [ "${1:-}" = --version ]; then\n'
    printf "  printf 'stackpulse 0.1.0 %s\\\\n'\n" "${STACKPULSE_TEST_BUILD_LABEL:-A}"
    cat <<'BINARY'
elif [ "${3:-}" = plugins ] && [ "${4:-}" = select ]; then
    printf '%s\n' "$@" > "$STACKPULSE_TEST_SELECTION_ARGS"
    pwd -P > "$STACKPULSE_TEST_SELECTION_CWD"
    printf 'plugins select\n' >> "$STACKPULSE_TEST_SELECTION_LOG"
    [ "$#" -eq 10 ] || exit 96
    [ "${5:-}" = --memory ] && [ "${7:-}" = --usagebar ] && [ "${9:-}" = --result-file ] || exit 96
    selection_result=${10}
    [ ! -e "$selection_result" ] && [ ! -L "$selection_result" ] || exit 97
    printf '%s\n' "$selection_result" > "$STACKPULSE_TEST_SELECTION_RESULT_PATH"
    [ "${STACKPULSE_TEST_SELECTION_EXIT:-0}" = 0 ] || exit "$STACKPULSE_TEST_SELECTION_EXIT"
    selection_memory=$6
    selection_usagebar=$8
    [ "$selection_memory" != select ] || selection_memory=${STACKPULSE_TEST_SELECTION_MEMORY:-with}
    [ "$selection_usagebar" != select ] || selection_usagebar=${STACKPULSE_TEST_SELECTION_USAGEBAR:-without}
    case "${STACKPULSE_TEST_SELECTION_RESULT:-valid}" in
        valid) printf 'memory=%s\nusagebar=%s\n' "$selection_memory" "$selection_usagebar" > "$selection_result" ;;
        missing) ;;
        malformed) printf 'memory=with\nusagebar=$(touch injected-selection)\n' > "$selection_result" ;;
        extra) printf 'memory=%s\nusagebar=%s\nextra\n' "$selection_memory" "$selection_usagebar" > "$selection_result" ;;
        trailing) printf 'memory=%s\nusagebar=%s\nextra' "$selection_memory" "$selection_usagebar" > "$selection_result" ;;
        incomplete) printf 'memory=%s\nusagebar=%s' "$selection_memory" "$selection_usagebar" > "$selection_result" ;;
        nul) printf 'memory=%s\000\nusagebar=%s\n' "$selection_memory" "$selection_usagebar" > "$selection_result" ;;
        override) printf 'memory=with\nusagebar=without\n' > "$selection_result" ;;
        override_usagebar) printf 'memory=without\nusagebar=with\n' > "$selection_result" ;;
        *) exit 98 ;;
    esac
elif [ "${3:-}" = memory ] && [ "${4:-}" = install ]; then
    [ "$#" -eq 6 ] || exit 99
    printf '%s\n' "$@" > "$STACKPULSE_TEST_MEMORY_ARGS"
    pwd -P > "$STACKPULSE_TEST_MEMORY_CWD"
    printf 'memory install\n' >> "$STACKPULSE_TEST_MEMORY_LOG"
    if [ "${STACKPULSE_TEST_MEMORY_EXIT:-0}" != 0 ] && [ "${STACKPULSE_TEST_MEMORY_EXIT:-0}" != 10 ]; then
        printf 'Erro: falha simulada ao instalar AI-Memory.\n' >&2
    fi
    exit "${STACKPULSE_TEST_MEMORY_EXIT:-0}"
elif [ "${3:-}" = usagebar ] && [ "${4:-}" = supported ]; then
    printf '%s\n' "$@" > "$STACKPULSE_TEST_USAGEBAR_SUPPORT_ARGS"
    printf 'usagebar supported\n' >> "$STACKPULSE_TEST_USAGEBAR_SUPPORT_LOG"
    exit "${STACKPULSE_TEST_USAGEBAR_SUPPORTED_EXIT:-1}"
elif [ "${3:-}" = usagebar ] && [ "${4:-}" = install ]; then
    [ "$#" -eq 6 ] || exit 99
    printf '%s\n' "$@" > "$STACKPULSE_TEST_USAGEBAR_ARGS"
    pwd -P > "$STACKPULSE_TEST_USAGEBAR_CWD"
    printf 'usagebar install\n' >> "$STACKPULSE_TEST_USAGEBAR_LOG"
    usagebar_exit=${STACKPULSE_TEST_USAGEBAR_EXIT:-0}
    if [ "$usagebar_exit" != 0 ] && [ "$usagebar_exit" != 10 ]; then
        printf 'Erro: falha simulada ao instalar AI-UsageBar.\n' >&2
    fi
    exit "$usagebar_exit"
else
    printf '%s\n' "$@"
fi
BINARY
} > "$target/release/ai-token-timeline"
chmod +x "$target/release/ai-token-timeline"
CARGO
chmod +x "$tools_dir/cargo"

test_path="$tools_dir:$system_tools"
STACKPULSE_TEST_LOG="$work/builds"
STACKPULSE_TEST_MEMORY_ARGS="$work/memory.args"
STACKPULSE_TEST_MEMORY_CWD="$work/memory.cwd"
STACKPULSE_TEST_MEMORY_LOG="$work/memory.log"
STACKPULSE_TEST_USAGEBAR_SUPPORT_ARGS="$work/usagebar-support.args"
STACKPULSE_TEST_USAGEBAR_SUPPORT_LOG="$work/usagebar-support.log"
STACKPULSE_TEST_USAGEBAR_ARGS="$work/usagebar.args"
STACKPULSE_TEST_USAGEBAR_CWD="$work/usagebar.cwd"
STACKPULSE_TEST_USAGEBAR_LOG="$work/usagebar.log"
STACKPULSE_TEST_SELECTION_ARGS="$work/selection.args"
STACKPULSE_TEST_SELECTION_CWD="$work/selection.cwd"
STACKPULSE_TEST_SELECTION_LOG="$work/selection.log"
STACKPULSE_TEST_SELECTION_RESULT_PATH="$work/selection-result.path"
export STACKPULSE_TEST_LOG STACKPULSE_TEST_MEMORY_ARGS STACKPULSE_TEST_MEMORY_CWD STACKPULSE_TEST_MEMORY_LOG
export STACKPULSE_TEST_USAGEBAR_SUPPORT_ARGS STACKPULSE_TEST_USAGEBAR_SUPPORT_LOG
export STACKPULSE_TEST_USAGEBAR_ARGS STACKPULSE_TEST_USAGEBAR_CWD STACKPULSE_TEST_USAGEBAR_LOG
export STACKPULSE_TEST_SELECTION_ARGS STACKPULSE_TEST_SELECTION_CWD STACKPULSE_TEST_SELECTION_LOG STACKPULSE_TEST_SELECTION_RESULT_PATH
: > "$STACKPULSE_TEST_LOG"
: > "$STACKPULSE_TEST_MEMORY_LOG"
: > "$STACKPULSE_TEST_USAGEBAR_SUPPORT_LOG"
: > "$STACKPULSE_TEST_USAGEBAR_LOG"
: > "$STACKPULSE_TEST_SELECTION_LOG"

install_with_memory() {
    (cd "$elsewhere" && PATH="$test_path" sh "$repo/setup.sh" "$@") < /dev/null > "$work/output" 2>&1
}

install() {
    install_with_memory --without-memory "$@"
}

expect_output() {
    grep -Fq -- "$1" "$work/output" || fail "missing installation guidance: $1"
}

reject_output() {
    if grep -Fq -- "$1" "$work/output"; then
        fail "unexpected installation guidance: $1"
    fi
}

expect_completion() {
    expect_output 'STACKPULSE'
    expect_output 'Instalação concluída'
    expect_output "$1"
    # A redirected install must remain readable even with a color-capable TERM.
    reject_output "$(printf '\033')"
}

expect_failure() {
    if install "$@"; then
        fail "command unexpectedly succeeded: $*"
    fi
    reject_output 'Instalação concluída'
}

# Special characters are intentional: neither install nor a later shell may
# evaluate a path as shell code.
special='with spaces '\''quote $(touch injected-dollar) `touch injected-backtick`'
prefix="$work/$special"
rc="$work/shell config '\''literal"
printf '# Existing shell settings\nSTACKPULSE_TEST_EXISTING=preserved\n' > "$rc"
(unset NO_COLOR; TERM=xterm-256color install --prefix "$prefix" --shell-rc "$rc") || fail 'first install'
binary="$prefix/bin/stackpulse"
manifest="$prefix/share/stackpulse/install.manifest"
[ -x "$binary" ] || fail 'installed binary is not executable'
[ "$("$binary" --version)" = 'stackpulse 0.1.0 A' ] || fail 'installed binary did not run'
[ -f "$manifest" ] || fail 'install manifest is missing'
[ "$(sed -n '1p' "$manifest")" = stackpulse-installer-v1 ] || fail 'install manifest has no ownership magic'
[ "$(sed -n '3p' "$manifest")" = "$(CDPATH= cd -- "$repo" && pwd -P)" ] || fail 'install manifest did not record the canonical source directory'
[ "$("$binary" 'prompt with spaces' '$(literal)' '`literal`')" = "$(printf '%s\n' 'prompt with spaces' '$(literal)' '`literal`')" ] || fail 'installed command changed arguments'
[ ! -e "$elsewhere/injected-dollar" ] && [ ! -e "$elsewhere/injected-backtick" ] || fail 'install evaluated a path'
expect_completion 'Preparado para novos terminais'
expect_output '0.1.0 A'
expect_output "$binary"
reject_output 'Disponível neste terminal'
reject_output 'Ativação manual'
# Exercise the command the user will copy, including all literal path quotes.
sed -n 's/^[[:space:]]*\(export PATH=.*\)$/\1/p' "$work/output" > "$work/activate.sh"
[ "$(sed -n '$=' "$work/activate.sh")" = 1 ] || fail 'expected exactly one copyable PATH activation command'
(
    cd "$elsewhere"
    PATH=/usr/bin:/bin
    . "$work/activate.sh"
    [ "$(command -v stackpulse)" = "$binary" ]
) || fail 'the displayed activation command did not preserve its literal path'
[ ! -e "$elsewhere/injected-dollar" ] && [ ! -e "$elsewhere/injected-backtick" ] || fail 'displayed activation command evaluated a path'
pass 'installs an executable from another directory, with literal special characters'

cp "$rc" "$work/rc.before"
cp "$manifest" "$work/manifest.before"
install --prefix "$prefix" --shell-rc "$rc" || fail 'repeat install'
cmp -s "$rc" "$work/rc.before" || fail 'repeat install duplicated or changed shell setup'
cmp -s "$manifest" "$work/manifest.before" || fail 'identical build changed its manifest'
(
    cd "$elsewhere"
    PATH=/usr/bin:/bin
    . "$rc"
    . "$rc"
    [ "${STACKPULSE_TEST_EXISTING:-}" = preserved ] || exit 1
    [ "$(command -v stackpulse)" = "$binary" ] || exit 2
    count=0
    rest=$PATH:
    while [ -n "$rest" ]; do
        entry=${rest%%:*}
        rest=${rest#*:}
        [ "$entry" != "$prefix/bin" ] || count=$((count + 1))
    done
    [ "$count" -eq 1 ] || exit 3
) || fail 'shell setup did not load safely and idempotently'
[ ! -e "$elsewhere/injected-dollar" ] && [ ! -e "$elsewhere/injected-backtick" ] || fail 'shell setup evaluated a path'
pass 'repeat install preserves existing shell configuration; sourcing twice adds PATH once'

(STACKPULSE_TEST_BUILD_LABEL=B install --prefix "$prefix" --shell-rc "$rc") || fail 'managed upgrade'
[ "$("$binary" --version)" = 'stackpulse 0.1.0 B' ] || fail 'managed installation did not upgrade'
expect_output '0.1.0 B'
cp "$binary" "$work/binary.before"
cp "$manifest" "$work/manifest.before"
cp "$rc" "$work/rc.before"
(STACKPULSE_TEST_FAIL_BUILD=1 expect_failure --prefix "$prefix" --shell-rc "$rc")
cmp -s "$binary" "$work/binary.before" || fail 'failed build changed installed executable'
cmp -s "$manifest" "$work/manifest.before" || fail 'failed build changed install manifest'
cmp -s "$rc" "$work/rc.before" || fail 'failed build changed shell configuration'
pass 'managed upgrades work; a failed build preserves the previous installation'

foreign="$work/foreign"
mkdir -p "$foreign/bin"
printf 'unrelated program\n' > "$foreign/bin/stackpulse"
cp "$foreign/bin/stackpulse" "$work/foreign.before"
expect_failure --prefix "$foreign" --no-path
cmp -s "$foreign/bin/stackpulse" "$work/foreign.before" || fail 'foreign executable was overwritten'
[ ! -e "$foreign/share/stackpulse/install.manifest" ] || fail 'failed install claimed a foreign executable'
install --prefix "$foreign" --no-path --force || fail 'explicit overwrite'
[ "$("$foreign/bin/stackpulse" --version)" = 'stackpulse 0.1.0 A' ] || fail 'force did not install the executable'
pass 'foreign commands are preserved unless --force is explicit'

printf 'locally changed executable\n' > "$foreign/bin/stackpulse"
cp "$foreign/bin/stackpulse" "$work/foreign.before"
expect_failure --prefix "$foreign" --no-path
cmp -s "$foreign/bin/stackpulse" "$work/foreign.before" || fail 'modified managed executable was overwritten'
pass 'a modified installed executable needs explicit overwrite'

no_path="$work/no path"
install --prefix "$no_path" --no-path || fail 'install with --no-path'
[ -x "$no_path/bin/stackpulse" ] || fail '--no-path skipped binary installation'
cmp -s "$rc" "$work/rc.before" || fail '--no-path changed shell configuration'
expect_completion 'Ativação manual'
expect_output 'export PATH='
reject_output 'Preparado para novos terminais'
reject_output 'Disponível neste terminal'
pass '--no-path installs without editing shell configuration'

cp "$STACKPULSE_TEST_LOG" "$work/builds.before"
help_prefix="$work/help only"
help_rc="$work/help.rc"
install --prefix "$help_prefix" --shell-rc "$help_rc" --help || fail '--help'
[ ! -e "$help_prefix" ] && [ ! -e "$help_rc" ] || fail '--help changed files'
reject_output 'Instalação concluída'
expect_failure --not-a-real-option
expect_failure --prefix
expect_failure --shell-rc
expect_failure --prefix "$work/bad:prefix"
expect_failure --no-path --shell-rc "$help_rc"
cmp -s "$STACKPULSE_TEST_LOG" "$work/builds.before" || fail 'help or invalid options started a build'
pass 'help and invalid options do not build or install'

unknown_shell_prefix="$work/no shell"
# Some /bin/sh implementations recreate an unset SHELL from the account record.
# Empty/unsupported values keep this check deterministic and away from real RCs.
(SHELL= install --prefix "$unknown_shell_prefix") || fail 'an empty SHELL prevented installation'
[ -x "$unknown_shell_prefix/bin/stackpulse" ] || fail 'an empty SHELL skipped binary installation'
grep -q 'export PATH=' "$work/output" || fail 'an empty SHELL omitted manual PATH instructions'
expect_completion 'Ativação manual'
reject_output 'Preparado para novos terminais'
reject_output 'Disponível neste terminal'
(SHELL=/unrecognized/terminal-shell install --prefix "$unknown_shell_prefix") || fail 'an unsupported SHELL prevented installation'
expect_completion 'Ativação manual'
expect_output 'export PATH='
reject_output 'Preparado para novos terminais'
reject_output 'Disponível neste terminal'
pass 'an empty or unsupported SHELL still installs and prints manual PATH instructions'

present_prefix="$work/already on path"
present_rc="$work/already-on-path.rc"
(test_path="$present_prefix/bin:$test_path"; install --prefix "$present_prefix" --shell-rc "$present_rc") || fail 'an existing PATH entry prevented installation'
[ ! -e "$present_rc" ] || fail 'an existing PATH entry unnecessarily changed shell configuration'
expect_completion 'Disponível neste terminal'
reject_output 'export PATH='
reject_output 'Preparado para novos terminais'
reject_output 'Ativação manual'
pass 'an existing PATH entry does not change shell configuration'

symlink_prefix="$work/symlink installation"
symlink_target="$work/unrelated target"
mkdir -p "$symlink_prefix/bin"
printf 'preserve target\n' > "$symlink_target"
ln -s "$symlink_target" "$symlink_prefix/bin/stackpulse"
expect_failure --prefix "$symlink_prefix" --no-path
[ -L "$symlink_prefix/bin/stackpulse" ] || fail 'an unapproved symlink was removed'
install --prefix "$symlink_prefix" --no-path --force || fail 'an explicit symlink replacement failed'
[ ! -L "$symlink_prefix/bin/stackpulse" ] || fail '--force did not replace the symlink'
[ "$(cat "$symlink_target")" = 'preserve target' ] || fail '--force modified the symlink target'
pass '--force replaces a command symlink without modifying its target'

missing="$work/missing cargo"
if (
    cd "$elsewhere"
    PATH="$system_tools" CARGO_HOME="$work/absent-cargo" \
        sh "$repo/setup.sh" --prefix "$missing" --no-path --without-memory
) > "$work/output" 2>&1; then
    fail 'missing Cargo unexpectedly succeeded'
fi
grep -iq cargo "$work/output" || fail 'missing Cargo error does not explain the prerequisite'
[ ! -e "$missing/bin/stackpulse" ] || fail 'missing Cargo installed a command'
reject_output 'Instalação concluída'
pass 'missing Cargo reports the prerequisite without installing Rust'

fallback="$work/cargo fallback"
mkdir -p "$fallback/bin"
cp "$tools_dir/cargo" "$fallback/bin/cargo"
fallback_prefix="$work/fallback installation"
(
    cd "$elsewhere"
    PATH="$system_tools" CARGO_HOME="$fallback" \
        sh "$repo/setup.sh" --prefix "$fallback_prefix" --no-path --without-memory
) > "$work/output" 2>&1 || fail 'Cargo outside PATH was not discovered in CARGO_HOME'
[ -x "$fallback_prefix/bin/stackpulse" ] || fail 'Cargo fallback did not install'
pass 'Cargo is discovered under CARGO_HOME when absent from PATH'

blocked="$work/prefix is a file"
printf 'preserve me\n' > "$blocked"
expect_failure --prefix "$blocked" --no-path
[ "$(cat "$blocked")" = 'preserve me' ] || fail 'invalid prefix was changed'
pass 'an unusable installation destination fails without destroying it'

[ ! -s "$STACKPULSE_TEST_MEMORY_LOG" ] || fail '--without-memory unexpectedly invoked memory installation'
expect_memory_args() {
    printf '%s\n' --allow-workspace "$(CDPATH= cd -- "$elsewhere" && pwd -P)" memory install --prefix "$1" > "$work/memory.expected"
    cmp -s "$STACKPULSE_TEST_MEMORY_ARGS" "$work/memory.expected" || fail 'memory installation received incorrect or reinterpreted arguments'
    [ "$(cat "$STACKPULSE_TEST_MEMORY_CWD")" = "$(CDPATH= cd -- "$elsewhere" && pwd -P)" ] || fail 'memory installation changed the invoking directory'
}

expect_selection_args() {
    selection_path=$(cat "$STACKPULSE_TEST_SELECTION_RESULT_PATH")
    printf '%s\n' --allow-workspace "$(CDPATH= cd -- "$elsewhere" && pwd -P)" plugins select --memory "$1" --usagebar "$2" --result-file "$selection_path" > "$work/selection.expected"
    cmp -s "$STACKPULSE_TEST_SELECTION_ARGS" "$work/selection.expected" || fail 'plugin selection received incorrect modes, literal path or arguments'
    [ "$(cat "$STACKPULSE_TEST_SELECTION_CWD")" = "$(CDPATH= cd -- "$elsewhere" && pwd -P)" ] || fail 'plugin selection changed the invoking directory'
    case "$selection_path" in
        /*) ;;
        *) fail 'selection result path must be absolute' ;;
    esac
    [ ! -e "$(dirname -- "$selection_path")" ] || fail 'selection temporary directory was not cleaned up'
}

memory_prefix="$work/memory $special"
install_with_memory --prefix "$memory_prefix" --no-path || fail 'default memory installation'
expect_memory_args "$memory_prefix"
expect_selection_args select without
[ "$(sed -n '$=' "$STACKPULSE_TEST_SELECTION_LOG")" = 1 ] || fail 'default memory installation opened more than one plugin selection'
expect_completion 'AI-Memory disponível'
expect_output 'Conexão com os CLIs é uma etapa separada.'
expect_output 'Criado por Fabio Akita (AkitaOnRails)'
expect_output 'https://github.com/akitaonrails/ai-memory'
expect_output 'https://akitaonrails.com'
reject_output 'Memória ativa'
[ ! -e "$elsewhere/injected-dollar" ] && [ ! -e "$elsewhere/injected-backtick" ] || fail 'memory installation evaluated a path'
pass 'default installation opens one plugin selection and installs without a second confirmation'

cp "$STACKPULSE_TEST_SELECTION_LOG" "$work/selection.before"
install_with_memory --prefix "$memory_prefix" --no-path --with-memory || fail 'explicit memory installation'
expect_memory_args "$memory_prefix"
expect_completion 'AI-Memory disponível'
cmp -s "$STACKPULSE_TEST_SELECTION_LOG" "$work/selection.before" || fail 'fixed options unnecessarily opened selection'
pass '--with-memory installs without opening the checkbox selection'

cp "$STACKPULSE_TEST_MEMORY_LOG" "$work/memory.before"
install_with_memory --prefix "$memory_prefix" --no-path --without-memory || fail 'explicit memory opt-out'
cmp -s "$STACKPULSE_TEST_MEMORY_LOG" "$work/memory.before" || fail '--without-memory invoked memory installation'
expect_completion 'AI-Memory · não selecionado'
reject_output 'AI-Memory disponível'
pass '--without-memory does not invoke memory installation'

cp "$STACKPULSE_TEST_MEMORY_LOG" "$work/memory.before"
(STACKPULSE_TEST_SELECTION_MEMORY=without install_with_memory --prefix "$memory_prefix" --no-path) || fail 'unchecking memory must still complete installation'
expect_selection_args select without
cmp -s "$STACKPULSE_TEST_MEMORY_LOG" "$work/memory.before" || fail 'unchecking memory still invoked installation'
expect_completion 'AI-Memory · não selecionado'
reject_output 'AI-Memory disponível'
reject_output 'instalação falhou'
pass 'unchecking memory in the shared selection completes StackPulse without installing it'

cp "$memory_prefix/bin/stackpulse" "$work/memory.binary.before"
cp "$memory_prefix/share/stackpulse/install.manifest" "$work/memory.manifest.before"
memory_failure=0
(STACKPULSE_TEST_MEMORY_EXIT=7 install_with_memory --prefix "$memory_prefix" --no-path --with-memory) || memory_failure=$?
[ "$memory_failure" -eq 7 ] || fail 'an optional dependency failure did not return its nonzero status'
expect_output 'Erro: falha simulada ao instalar AI-Memory.'
expect_output 'StackPulse instalado'
expect_output 'AI-Memory · instalação falhou'
expect_output 'StackPulse foi instalado e pode ser usado.'
reject_output 'Instalação concluída'
reject_output 'AI-Memory disponível'
cmp -s "$memory_prefix/bin/stackpulse" "$work/memory.binary.before" || fail 'optional memory failure changed the valid StackPulse binary'
cmp -s "$memory_prefix/share/stackpulse/install.manifest" "$work/memory.manifest.before" || fail 'optional memory failure changed the StackPulse ownership manifest'
[ "$("$memory_prefix/bin/stackpulse" --version)" = 'stackpulse 0.1.0 A' ] || fail 'StackPulse stopped working after optional memory failure'
pass 'a memory installation failure preserves a usable StackPulse and clearly reports partial completion'

cp "$STACKPULSE_TEST_LOG" "$work/builds.before"
cp "$STACKPULSE_TEST_MEMORY_LOG" "$work/memory.before"
for flags in '--with-memory --without-memory' '--without-memory --with-memory'; do
    if install_with_memory $flags --prefix "$memory_prefix" --no-path; then
        fail 'conflicting memory flags unexpectedly succeeded'
    fi
    expect_output '--with-memory e --without-memory não podem ser combinados.'
    reject_output 'Instalação concluída'
done
cmp -s "$STACKPULSE_TEST_LOG" "$work/builds.before" || fail 'conflicting memory flags started a build'
cmp -s "$STACKPULSE_TEST_MEMORY_LOG" "$work/memory.before" || fail 'conflicting memory flags invoked memory installation'
pass 'conflicting memory flags fail before building or installing'

[ ! -s "$STACKPULSE_TEST_USAGEBAR_LOG" ] || fail 'unsupported fixtures unexpectedly invoked AI-UsageBar installation'
expect_usagebar_args() {
    printf '%s\n' --allow-workspace "$(CDPATH= cd -- "$elsewhere" && pwd -P)" usagebar install --prefix "$1" > "$work/usagebar.expected"
    cmp -s "$STACKPULSE_TEST_USAGEBAR_ARGS" "$work/usagebar.expected" || fail 'UsageBar installation received incorrect or reinterpreted arguments'
    [ "$(cat "$STACKPULSE_TEST_USAGEBAR_CWD")" = "$(CDPATH= cd -- "$elsewhere" && pwd -P)" ] || fail 'UsageBar installation changed the invoking directory'
}

usagebar_prefix="$work/usagebar $special"
cp "$STACKPULSE_TEST_USAGEBAR_LOG" "$work/usagebar.before"
install --prefix "$usagebar_prefix" --no-path || fail 'unsupported default UsageBar installation'
printf '%s\n' --allow-workspace "$(CDPATH= cd -- "$elsewhere" && pwd -P)" usagebar supported > "$work/usagebar-support.expected"
cmp -s "$STACKPULSE_TEST_USAGEBAR_SUPPORT_ARGS" "$work/usagebar-support.expected" || fail 'compatibility probe received incorrect arguments'
cmp -s "$STACKPULSE_TEST_USAGEBAR_LOG" "$work/usagebar.before" || fail 'an unsupported platform invoked UsageBar selection or installation'
expect_completion 'Ativação manual'
reject_output 'AI-UsageBar'
reject_output 'https://github.com/akitaonrails/ai-usagebar'
pass 'unsupported platforms probe read-only and omit UsageBar installation and summary'

cp "$STACKPULSE_TEST_MEMORY_LOG" "$work/memory.before"
(STACKPULSE_TEST_USAGEBAR_SUPPORTED_EXIT=0 install --prefix "$usagebar_prefix" --no-path) || fail 'compatible default UsageBar selection'
expect_selection_args without select
cmp -s "$STACKPULSE_TEST_USAGEBAR_LOG" "$work/usagebar.before" || fail 'unchecked default UsageBar invoked installation'
expect_completion 'AI-UsageBar · não selecionado'
expect_output 'https://github.com/akitaonrails/ai-usagebar'
expect_output 'Criado por Fabio Akita (AkitaOnRails)'
expect_output 'https://akitaonrails.com'
reject_output 'AI-UsageBar disponível'
cmp -s "$STACKPULSE_TEST_MEMORY_LOG" "$work/memory.before" || fail '--without-memory invoked memory while selecting UsageBar'
pass 'compatible platforms offer an unchecked UsageBar selection independently of --without-memory'

(STACKPULSE_TEST_USAGEBAR_SUPPORTED_EXIT=0 STACKPULSE_TEST_SELECTION_USAGEBAR=with install --prefix "$usagebar_prefix" --no-path) || fail 'selecting UsageBar'
expect_usagebar_args "$usagebar_prefix"
expect_selection_args without select
expect_completion 'AI-UsageBar disponível'
expect_output 'Acesso pelo CLI e TUI, sem autostart ou menu bar.'
pass 'selecting UsageBar records availability without enabling autostart or menu bar'

cp "$STACKPULSE_TEST_SELECTION_LOG" "$work/selection.before"
(STACKPULSE_TEST_USAGEBAR_SUPPORTED_EXIT=0 install --prefix "$usagebar_prefix" --no-path --with-usagebar) || fail 'explicit UsageBar installation'
expect_usagebar_args "$usagebar_prefix"
expect_completion 'AI-UsageBar disponível'
cmp -s "$STACKPULSE_TEST_SELECTION_LOG" "$work/selection.before" || fail 'two explicit flags unnecessarily opened plugin selection'
[ ! -e "$elsewhere/injected-dollar" ] && [ ! -e "$elsewhere/injected-backtick" ] || fail 'UsageBar installation evaluated a path'
pass '--with-usagebar installs directly with literal paths and the invoking directory'

cp "$STACKPULSE_TEST_USAGEBAR_LOG" "$work/usagebar.before"
cp "$STACKPULSE_TEST_USAGEBAR_SUPPORT_LOG" "$work/usagebar-support.before"
install_with_memory --prefix "$usagebar_prefix" --no-path --without-usagebar --with-memory || fail 'explicit UsageBar opt-out with memory enabled'
expect_memory_args "$usagebar_prefix"
expect_completion 'AI-Memory disponível'
expect_output 'AI-UsageBar · não selecionado'
cmp -s "$STACKPULSE_TEST_USAGEBAR_LOG" "$work/usagebar.before" || fail '--without-usagebar invoked installation'
cmp -s "$STACKPULSE_TEST_USAGEBAR_SUPPORT_LOG" "$work/usagebar-support.before" || fail '--without-usagebar needlessly probed support'
pass '--without-usagebar skips UsageBar and preserves independent AI-Memory installation'

usagebar_failure=0
install --prefix "$usagebar_prefix" --no-path --with-usagebar || usagebar_failure=$?
[ "$usagebar_failure" -eq 1 ] || fail '--with-usagebar on unsupported platform did not fail'
expect_output 'AI-UsageBar não é compatível com este sistema.'
expect_output 'Nenhum download de AI-UsageBar foi iniciado.'
expect_output 'AI-UsageBar · sistema incompatível'
expect_output 'StackPulse instalado'
expect_output 'https://github.com/akitaonrails/ai-usagebar'
reject_output 'Instalação concluída'
cmp -s "$STACKPULSE_TEST_USAGEBAR_LOG" "$work/usagebar.before" || fail 'unsupported explicit UsageBar request invoked installation'
[ "$("$usagebar_prefix/bin/stackpulse" --version)" = 'stackpulse 0.1.0 A' ] || fail 'unsupported UsageBar request left StackPulse unusable'
pass 'explicit unsupported UsageBar requests fail clearly without downloads and preserve StackPulse'

usagebar_failure=0
(STACKPULSE_TEST_USAGEBAR_SUPPORTED_EXIT=6 install --prefix "$usagebar_prefix" --no-path --with-usagebar) || usagebar_failure=$?
[ "$usagebar_failure" -eq 6 ] || fail 'compatibility probe errors were treated as a supported platform'
expect_output 'não foi possível verificar a compatibilidade de AI-UsageBar'
cmp -s "$STACKPULSE_TEST_USAGEBAR_LOG" "$work/usagebar.before" || fail 'a failed compatibility probe invoked UsageBar installation'
pass 'failed compatibility probes preserve their error code and never invoke installation'

usagebar_failure=0
(STACKPULSE_TEST_USAGEBAR_SUPPORTED_EXIT=0 STACKPULSE_TEST_USAGEBAR_EXIT=8 install_with_memory --prefix "$usagebar_prefix" --no-path --with-memory --with-usagebar) || usagebar_failure=$?
[ "$usagebar_failure" -eq 8 ] || fail 'UsageBar installation failure lost its exit code'
expect_output 'AI-Memory disponível'
expect_output 'AI-UsageBar · instalação falhou'
expect_output 'Erro: falha simulada ao instalar AI-UsageBar.'
expect_output 'StackPulse instalado'
reject_output 'Instalação concluída'
pass 'UsageBar failure preserves successful AI-Memory and StackPulse installation'

memory_failure=0
(STACKPULSE_TEST_USAGEBAR_SUPPORTED_EXIT=0 STACKPULSE_TEST_MEMORY_EXIT=7 install_with_memory --prefix "$usagebar_prefix" --no-path --with-memory --with-usagebar) || memory_failure=$?
[ "$memory_failure" -eq 7 ] || fail 'memory failure lost its exit code while installing UsageBar'
expect_output 'AI-Memory · instalação falhou'
expect_output 'AI-UsageBar disponível'
expect_usagebar_args "$usagebar_prefix"
reject_output 'Instalação concluída'
pass 'AI-Memory failure does not suppress a successful UsageBar installation'

combined_failure=0
(STACKPULSE_TEST_USAGEBAR_SUPPORTED_EXIT=0 STACKPULSE_TEST_USAGEBAR_EXIT=8 STACKPULSE_TEST_MEMORY_EXIT=7 install_with_memory --prefix "$usagebar_prefix" --no-path --with-memory --with-usagebar) || combined_failure=$?
[ "$combined_failure" -eq 7 ] || fail 'combined failures did not preserve the first installation failure'
expect_output 'AI-Memory · instalação falhou'
expect_output 'AI-UsageBar · instalação falhou'
expect_output 'StackPulse foi instalado e pode ser usado.'
[ "$("$usagebar_prefix/bin/stackpulse" --version)" = 'stackpulse 0.1.0 A' ] || fail 'combined optional failures left StackPulse unusable'
pass 'combined optional failures report both outcomes and retain a usable StackPulse'

cp "$STACKPULSE_TEST_LOG" "$work/builds.before"
cp "$STACKPULSE_TEST_USAGEBAR_LOG" "$work/usagebar.before"
cp "$STACKPULSE_TEST_USAGEBAR_SUPPORT_LOG" "$work/usagebar-support.before"
for flags in '--with-usagebar --without-usagebar' '--without-usagebar --with-usagebar'; do
    if install_with_memory $flags --prefix "$usagebar_prefix" --no-path; then
        fail 'conflicting UsageBar flags unexpectedly succeeded'
    fi
    expect_output '--with-usagebar e --without-usagebar não podem ser combinados.'
    reject_output 'Instalação concluída'
done
cmp -s "$STACKPULSE_TEST_LOG" "$work/builds.before" || fail 'conflicting UsageBar flags started a build'
cmp -s "$STACKPULSE_TEST_USAGEBAR_LOG" "$work/usagebar.before" || fail 'conflicting UsageBar flags invoked installation'
cmp -s "$STACKPULSE_TEST_USAGEBAR_SUPPORT_LOG" "$work/usagebar-support.before" || fail 'conflicting UsageBar flags probed support'
pass 'conflicting UsageBar flags fail before building or installing'

selection_temp="$work/selection $special"
mkdir -p "$selection_temp"
selections_before=$(sed -n '$=' "$STACKPULSE_TEST_SELECTION_LOG")
(TMPDIR="$selection_temp" STACKPULSE_TEST_USAGEBAR_SUPPORTED_EXIT=0 STACKPULSE_TEST_SELECTION_USAGEBAR=with install_with_memory --prefix "$usagebar_prefix" --no-path) || fail 'selecting both plugins on one page'
expect_selection_args select select
expect_memory_args "$usagebar_prefix"
expect_usagebar_args "$usagebar_prefix"
[ "$(sed -n '$=' "$STACKPULSE_TEST_SELECTION_LOG")" -eq "$((selections_before + 1))" ] || fail 'selecting both plugins opened multiple selection pages'
expect_completion 'AI-Memory disponível'
expect_output 'AI-UsageBar disponível'
[ ! -e "$elsewhere/injected-dollar" ] && [ ! -e "$elsewhere/injected-backtick" ] || fail 'selection evaluated a temporary result path'
pass 'one shared selection chooses both plugins and preserves literal temporary paths'

(STACKPULSE_TEST_USAGEBAR_SUPPORTED_EXIT=0 install_with_memory --prefix "$usagebar_prefix" --no-path --with-memory) || fail 'a fixed memory option hid editable UsageBar'
expect_selection_args with select
expect_memory_args "$usagebar_prefix"
expect_completion 'AI-Memory disponível'
expect_output 'AI-UsageBar · não selecionado'
pass 'an explicit memory flag stays fixed while UsageBar remains editable'

cp "$STACKPULSE_TEST_MEMORY_LOG" "$work/memory.before"
cp "$STACKPULSE_TEST_USAGEBAR_LOG" "$work/usagebar.before"
(STACKPULSE_TEST_USAGEBAR_SUPPORTED_EXIT=0 STACKPULSE_TEST_SELECTION_MEMORY=without STACKPULSE_TEST_SELECTION_USAGEBAR=without install_with_memory --prefix "$usagebar_prefix" --no-path) || fail 'dismissing both editable plugin options'
expect_selection_args select select
expect_completion 'AI-Memory · não selecionado'
expect_output 'AI-UsageBar · não selecionado'
cmp -s "$STACKPULSE_TEST_MEMORY_LOG" "$work/memory.before" || fail 'dismissed memory selection invoked installation'
cmp -s "$STACKPULSE_TEST_USAGEBAR_LOG" "$work/usagebar.before" || fail 'dismissed UsageBar selection invoked installation'
pass 'skipping both editable options completes StackPulse without plugin downloads'

selection_failure=0
(STACKPULSE_TEST_USAGEBAR_SUPPORTED_EXIT=0 STACKPULSE_TEST_SELECTION_EXIT=130 install_with_memory --prefix "$usagebar_prefix" --no-path --with-memory) || selection_failure=$?
[ "$selection_failure" -eq 130 ] || fail 'selection cancellation lost its exit code'
expect_selection_args with select
expect_output 'seleção de plugins encerrada sem confirmação'
expect_output 'Nenhum plugin foi instalado'
reject_output 'Instalação concluída'
cmp -s "$STACKPULSE_TEST_MEMORY_LOG" "$work/memory.before" || fail 'cancelled selection installed fixed memory'
cmp -s "$STACKPULSE_TEST_USAGEBAR_LOG" "$work/usagebar.before" || fail 'cancelled selection installed UsageBar'
pass 'aborting the shared selection prevents every plugin download and removes temporary output'

for result in missing malformed extra trailing incomplete nul; do
    selection_failure=0
    (STACKPULSE_TEST_USAGEBAR_SUPPORTED_EXIT=0 STACKPULSE_TEST_SELECTION_RESULT="$result" install_with_memory --prefix "$usagebar_prefix" --no-path) || selection_failure=$?
    [ "$selection_failure" -ne 0 ] || fail "invalid selection result unexpectedly succeeded: $result"
    expect_selection_args select select
    expect_output 'Nenhum plugin foi instalado'
    cmp -s "$STACKPULSE_TEST_MEMORY_LOG" "$work/memory.before" || fail "invalid selection result installed memory: $result"
    cmp -s "$STACKPULSE_TEST_USAGEBAR_LOG" "$work/usagebar.before" || fail "invalid selection result installed UsageBar: $result"
    [ ! -e "$elsewhere/injected-selection" ] || fail 'selection result was evaluated as shell code'
done
pass 'missing, malformed, extra, incomplete and NUL-containing results never install plugins or execute shell code'

selection_failure=0
(STACKPULSE_TEST_USAGEBAR_SUPPORTED_EXIT=0 STACKPULSE_TEST_SELECTION_RESULT=override install --prefix "$usagebar_prefix" --no-path) || selection_failure=$?
[ "$selection_failure" -ne 0 ] || fail 'selection overrode explicit --without-memory'
expect_output 'A seleção alterou uma opção fixa de AI-Memory'
expect_selection_args without select
selection_failure=0
(STACKPULSE_TEST_USAGEBAR_SUPPORTED_EXIT=0 STACKPULSE_TEST_SELECTION_RESULT=override_usagebar install_with_memory --prefix "$usagebar_prefix" --no-path --without-usagebar) || selection_failure=$?
[ "$selection_failure" -ne 0 ] || fail 'selection overrode explicit --without-usagebar'
expect_output 'A seleção alterou uma opção fixa de AI-UsageBar'
expect_selection_args select without
cmp -s "$STACKPULSE_TEST_MEMORY_LOG" "$work/memory.before" || fail 'a rejected fixed-option override installed memory'
cmp -s "$STACKPULSE_TEST_USAGEBAR_LOG" "$work/usagebar.before" || fail 'a rejected fixed-option override installed UsageBar'
pass 'fixed command-line choices cannot be overridden by a selection result'

printf 'Installer checks passed.\n'
