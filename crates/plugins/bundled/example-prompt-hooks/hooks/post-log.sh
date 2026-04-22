#!/bin/sh
# Example command hook that runs alongside the prompt hook in PostToolUse.
# Logs the tool name and timestamp to a local file.
printf '%s  %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$HOOK_TOOL_NAME" >> .anvil-hook-log.txt
