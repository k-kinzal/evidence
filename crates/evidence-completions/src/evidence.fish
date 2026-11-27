function __evidence_complete
    set -l args (commandline -opc)
    set -l current (commandline -ct)
    set -l context
    set -l after
    for word in $args[2..-1]
        if test -n "$after"
            set after
            continue
        end
        switch "$word"
            case -C --directory --color
                set after "$word"
            case '-*'
            case '*'
                if test (count $context) -eq 0
                    set context "$word"
                else if test "$context[1]" = config; and test (count $context) -eq 1
                    set -a context "$word"
                end
        end
    end
    if test (count $context) -gt 0; and not contains -- "$context[1]" (command evidence --list-cmds=builtins)
        set -l hook "_evidence_$context[1]"
        if functions --query "$hook"
            $hook
        end
        return
    end
    switch "$after"
        case -C --directory
            __fish_complete_directories "$current"
            return
        case --color
            printf '%s\n' auto always never
            return
    end
    if string match -q -- '-*' "$current"
        command evidence $context --completion-helper
    else if test (count $context) -eq 0; or begin
        test "$context[1]" = config; and test (count $context) -eq 1
    end
        command evidence $context --list-cmds
    else if contains -- "$context[1]" init update
        __fish_complete_path "$current"
    end
end
complete --command evidence --no-files --arguments '(__evidence_complete)'
