_evidence_complete() {
    local current="${COMP_WORDS[COMP_CWORD]}" cmd="" sub="" word i
    for ((i = 1; i < COMP_CWORD; i++)); do
        word=${COMP_WORDS[i]}
        case $word in
            -C|--directory|--color) ((i++)); continue ;;
            -*) continue ;;
        esac
        if [[ -z $cmd ]]; then
            cmd=$word
        elif [[ $cmd == config && -z $sub ]]; then
            sub=$word
        fi
    done
    if [[ -n $cmd ]] && ! command evidence --list-cmds=builtins | grep -Fxq -- "$cmd"; then
        if declare -F -- "_evidence_$cmd" >/dev/null; then
            "_evidence_$cmd"
        fi
        return
    fi

    local -a command_path=()
    [[ -z $cmd ]] || command_path+=("$cmd")
    [[ -z $sub ]] || command_path+=("$sub")
    case ${COMP_WORDS[COMP_CWORD-1]} in
        -C|--directory) _evidence_complete_paths -d; return ;;
        --color) _evidence_complete_lines printf '%s\n' auto always never; return ;;
    esac
    if [[ $current == -* ]]; then
        _evidence_complete_lines command evidence "${command_path[@]}" --completion-helper
    elif [[ -z $cmd || ( $cmd == config && -z $sub ) ]]; then
        _evidence_complete_lines command evidence "${command_path[@]}" --list-cmds
    elif [[ $cmd == init || $cmd == update ]]; then
        _evidence_complete_paths -f
    fi
}

_evidence_complete_lines() {
    local candidate current="${COMP_WORDS[COMP_CWORD]}"
    while IFS= read -r candidate; do
        if [[ $candidate == "$current"* ]]; then
            COMPREPLY+=("$candidate")
        fi
    done < <("$@")
}

_evidence_complete_paths() {
    compopt -o filenames 2>/dev/null || :
    _evidence_complete_lines compgen "$1" -- "${COMP_WORDS[COMP_CWORD]}"
}

complete -F _evidence_complete evidence
