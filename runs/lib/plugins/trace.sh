# Shared by the scripted plugins. Expects $dir to be the state directory.

# Milliseconds since the run began.
elapsed_ms() {
    now=$(date +%s%N)
    read -r start < "$dir/start"
    echo $(((now - start) / 1000000))
}

# Adds one line to the trace of plugin calls.
trace() {
    printf '%s %s %s\n' "$(elapsed_ms)" "$1" "$2" >> "$dir/trace"
}
