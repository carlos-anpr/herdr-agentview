# Explicar actividad con `e`

Selecciona un agente o un subagente y pulsa **e**. La explicación relaciona sus acciones recientes con el encargo, resume el avance y señala bloqueos. Las razones son inferencias a partir del transcript: no se presentan como pensamiento interno del agente.

La respuesta aparece progresivamente. Usa ↑/↓, RePág/AvPág o la rueda para desplazarte, **e** para actualizar la foto y **Esc** para cerrar y cancelar la consulta. Si la foto no cambia se reutiliza la explicación, con una caché acotada en memoria.

## Configuración opcional

Variables del proceso que arranca el plugin:

```sh
export AGENTVIEW_AI_MODEL=deepseek-v4.1-flash
export AGENTVIEW_AI_EFFORT=max
```

Sin overrides se usa DeepSeek V4.1 Flash, esfuerzo `max`, en OpenCode Go. El cuerpo lleva `reasoning_effort: "max"` y `thinking: {"type":"enabled"}`, siguiendo las [variantes de DeepSeek de OpenCode](https://github.com/anomalyco/opencode/blob/dev/packages/core/src/variant.ts).

- `AGENTVIEW_AI_API_URL`: endpoint OpenAI Chat Completions con streaming. Por defecto `https://opencode.ai/inference/go/openai/v1/chat/completions`.
- `AGENTVIEW_AI_API_KEY_VALUE`: clave opcional. Si se omite y se usa el endpoint predeterminado, se reutiliza la entrada `herdr-gitview` del almacén `$XDG_DATA_HOME/opencode/auth.json` o `~/.local/share/opencode/auth.json`; si falta, se busca `opencode-go`. No se copian claves a la configuración del proyecto. Un endpoint personalizado no recibe automáticamente esa credencial.
- Esfuerzos admitidos: `none`, `minimal`, `low`, `medium`, `high`, `max`, `xhigh`; el modelo debe admitir el valor seleccionado. DeepSeek Flash admite `low`, `high`, `max`, y `none` desactiva su razonamiento.

Las variables son opcionales; una variable definida pero vacía o inválida muestra el error al pedir la explicación. `.env.example` documenta las claves, pero no se carga automáticamente. Omite la variable de clave para utilizar el almacén existente.

## Alcance y comprobación

Sólo se consulta al pulsar `e`. Se envía una foto acotada de la tarea, plan, última respuesta y hasta 40 eventos; no se lee el repositorio, no se adjunta el transcript completo ni la pantalla y no se ejecutan herramientas. Las líneas que parecen contener credenciales se omiten y la clave de autenticación se elimina también del contexto si apareciera. La clave y el cuerpo de la petición viajan por stdin de `curl`, sin ficheros temporales ni secretos en sus argumentos. Tiempo máximo: 120 segundos.

Las pruebas habituales usan un servidor SSE local y datos sintéticos. Para una comprobación real explícita, también sintética:

```sh
cargo test ai::tests::opencode_go_smoke -- --ignored --nocapture
```
