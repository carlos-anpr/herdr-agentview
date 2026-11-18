#!/bin/sh
# Abre el centro de control como popup, con el contexto del panel desde el que se invoca.
H="${HERDR_BIN_PATH:-herdr}"
exec "$H" plugin pane open --plugin local.agentview --entrypoint viewer --placement popup \
  --width 94% --height 92% --env "HERDR_PLUGIN_CONTEXT_JSON=${HERDR_PLUGIN_CONTEXT_JSON:-}"
