# Environment variables

aivyx-coder is configured through `config.toml` and command-line flags, not
the environment. It reads only these:

| Variable | Effect |
|---|---|
| `AIVYX_DEBUG_LOG` | A file to append the raw traffic to and from the model to — plain text, append-only, no rotation. For debugging a model or server; treat the file as sensitive and delete it afterwards. |
| `XDG_CONFIG_HOME` | Where the config directory lives on Linux (default `~/.config`). Also used to recognise your global git config (`$XDG_CONFIG_HOME/git`) so the model can't write to it. |
| `HOME` | The base of the default paths. |
| `AIVYX_CODER_ACP_TERMINAL_AUTH` | Set by aivyx-coder itself when an editor runs `--setup` as its sign-in step; it only changes the final message. You never set it. |

## What confined commands see

Commands the model runs (`run_command`, `run_shell`, `/test`, git, REPLs,
MCP and language servers) inherit your environment, with one exception:
`SSH_AUTH_SOCK`, `GPG_AGENT_INFO`, `DBUS_SESSION_BUS_ADDRESS`,
`XDG_RUNTIME_DIR`, `WAYLAND_DISPLAY` and `DISPLAY` are removed (unless
`[sandbox] allow_unix_sockets = true`), and `TMPDIR` points at a private
temporary directory. Anything else in your environment — API tokens
included — is visible to them, so don't export secrets you wouldn't want a
command to read. See the [security model](05-security-model.md).

The model server's own settings (for example `OLLAMA_CONTEXT_LENGTH`) are
read by the server, not by aivyx-coder — see
[Local model servers](../guide/09-local-model-servers.md).
