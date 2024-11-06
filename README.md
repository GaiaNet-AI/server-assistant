# server-assistant

## CLI Options

```bash
$gaias --help

An assistant for LlamaEdge API Server

Usage: gaias [OPTIONS] --server-log-file <SERVER_LOG_FILE> --gaianet-dir <GAIANET_DIR>

Options:
      --server-socket-addr <SERVER_SOCKET_ADDR>
          Socket address of LlamaEdge API Server instance [default: 0.0.0.0:8080]
      --server-log-file <SERVER_LOG_FILE>
          Path to the `start-llamaedge.log` file
      --gaianet-dir <GAIANET_DIR>
          Path to gaianet directory
  -i, --interval <INTERVAL>
          Interval in seconds for sending notifications [default: 10]
      --system-prompt <SYSTEM_PROMPT>
          System prompt from config.json
      --rag-prompt <RAG_PROMPT>
          RAG prompt from config.json [default: ]
      --log <LOG>
          log file [default: assistant.log]
  -h, --help
          Print help
  -V, --version
          Print version
```
