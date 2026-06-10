# Digital Twin

A persistent memory layer for AI-assisted development. Digital Twin combines a **Neo4j knowledge graph** for structured memory (events, decisions, configurations) with a **Qdrant vector database** for semantic code search, enabling AI agents to maintain context across sessions.

---

## Architecture

```
┌─────────────────────────────────────────────────────────┐
│                    AI Agent (OpenCode)                    │
│  AGENTS.md triggers: dt event / dt memorize / dt update  │
└──────────────────────┬──────────────────────────────────┘
                       │ dt CLI
┌──────────────────────▼──────────────────────────────────┐
│                    dt (Rust CLI)                         │
│                                                         │
│  ┌──────────┐  ┌───────────┐  ┌────────┐  ┌─────────┐  │
│  │ dt event │  │ dt memorize│  │dt build│  │dt update│  │
│  │ dt remove│  │ dt search  │  │ index  │  │validate │  │
│  └─────┬────┘  └─────┬─────┘  └───┬────┘  └────┬────┘  │
│        │              │            │            │       │
│  ┌─────▼──────────────▼────────────▼────────────▼─────┐ │
│  │           tree-sitter (7 languages)                │ │
│  └───────────────────────┬───────────────────────────┘ │
└──────────────────────────┼─────────────────────────────┘
                           │
        ┌──────────────────┼──────────────────┐
        ▼                  ▼                  ▼
┌──────────────┐  ┌──────────────┐  ┌──────────────────┐
│   Neo4j      │  │   Qdrant     │  │   Embed Server    │
│  Knowledge   │  │   Vector     │  │  (Python + BGE)   │
│  Graph       │  │   Database   │  │  localhost:8001   │
│  localhost   │  │  localhost   │  └──────────────────┘
│  :7474       │  │  :6333       │
└──────────────┘  └──────────────┘
```

### Components

| Component | Language | Purpose |
|-----------|----------|---------|
| `engine-rust/` | Rust | Core CLI (`dt`): indexing, search, event/memory management |
| `services/embed-server/` | Python + sentence-transformers | Text embedding inference (BGE-base-zh-v1.5) |
| `services/search-web/` | Python + Flask | Web search UI |
| `config.yaml` | YAML | Central configuration |

---

## Requirements

### Runtime

| Service | Version | Purpose |
|---------|---------|---------|
| [Neo4j](https://neo4j.com/download/) | 5.x | Knowledge graph storage |
| [Qdrant](https://qdrant.tech/documentation/quick-start/) | 1.x | Vector database for semantic search |
| Python | 3.10+ | Embed server |
| Rust | 1.75+ | Building the `dt` CLI |

### System Dependencies

```bash
# Build essentials for Rust tree-sitter
sudo apt install build-essential cmake pkg-config

# Python dependencies (for embed server)
sudo apt install python3 python3-pip python3-venv
```

---

## Installation

### 1. Start Required Services

**Neo4j:**
```bash
# Native install (systemd)
sudo systemctl start neo4j

# Or download from https://neo4j.com/download/
```

**Qdrant:**
```bash
# Native install
curl -L https://github.com/qdrant/qdrant/releases/latest/download/qdrant-x86_64-unknown-linux-gnu.tar.gz | tar xz
./qdrant &

# Or via package manager
```

### 2. Embed Server

```bash
cd services/embed-server
python3 -m venv venv
source venv/bin/activate
pip install -r requirements.txt
python3 main.py
# Starts on http://localhost:8001
```

### 3. Build and Install `dt` CLI

```bash
cd engine-rust
cargo build --release
sudo cp target/release/dt /usr/local/bin/dt

# Verify
dt --help
```

### 4. Configure

```bash
cp config.yaml.example config.yaml
```

Edit `config.yaml` to match your environment.

The `dt` CLI reads `config.yaml` from the project root directory. Override with `DT_CONFIG` environment variable.

### 5. Install OpenCode Skill (Optional)

```bash
# The skill tells AI agents to query the knowledge graph automatically
mkdir -p ~/.opencode/skills/digital-twin
cp SKILL.md ~/.opencode/skills/digital-twin/SKILL.md

# Symlink AGENTS.md for AI behavior rules
ln -sf "$(pwd)/AGENTS.md" ~/AGENTS.md
```

Or run the setup script which does all of this automatically:

```bash
bash setup.sh
```

---

## Usage

### Code Indexing

```bash
# Full index (rebuild from scratch)
dt index --path /path/to/project --name my-project

# Incremental build (uses SQLite hash cache)
dt build --path /path/to/project --name my-project

# Index a single file (after editing)
dt update --path /path/to/project --name my-project --file src/main.py

# Remove a file from index
dt remove --project my-project --file src/old.py

# Remove entire project
dt remove --project my-project --all
```

### Knowledge Graph Operations

```bash
# Record an Event (e.g., deploy, config change)
dt event --type Deploy \
  --entity-id "user-center" \
  --entity-type JenkinsJob \
  --project "user-center" \
  --details "branch: main, env: production"

# Record a Knowledge entry (e.g., architecture decision)
dt memorize --type Decision \
  --entity-id "REST-to-gRPC" \
  --entity-type ArchitectureDecision \
  --project "user-center" \
  --details "decision: migrate to gRPC; reason: 10x lower latency; scope: user-service"
```

### Semantic Code Search

```bash
# Search within a project
dt search "user login flow" --project user-center

# Search all projects
dt search "payment timeout" --all --limit 20

# JSON output
dt search "refund logic" --project order-center --json

# Rebuild call graph relationships
dt build-call-graph --name user-center
```

### Nacos Configuration Sync

```bash
# Sync test environment (nacos.newoffen.net)
dt nacos-sync --env test

# Sync production environment (nacos.newoffen.com)
dt nacos-sync --env prod

# Sync both
dt nacos-sync --env all
```

### Utility

```bash
# Validate extraction quality (dry run, no DB writes)
dt validate --path /path/to/project --name my-project

# Parse a single file and output JSON
dt parse --file src/main.py --project my-project --root /path/to/project
```

---

## How Incremental Build Works

```
dt build --path /proj --name myapp
  │
  ├─ Scan project directory (ignores node_modules, .git, etc.)
  │
  ├─ Compute SHA1 hash for each file
  │
  ├─ Compare with SQLite cache (/var/lib/digital-twin/lazy.db)
  │   ├─ Hash matches → skip (unchanged)
  │   ├─ Hash differs → re-index
  │   └─ File in cache but not on disk → delete from Neo4j + Qdrant
  │
  ├─ For each changed file:
  │   1. tree-sitter parse → extract methods/classes
  │   2. Embed via HTTP → get 768-dim vector
  │   3. Write to Qdrant (vector + payload)
  │   4. Write to Neo4j (Method node + Class + CONTAINS)
  │   5. Update SQLite hash cache
  │
  └─ Rebuild CALLS relationships in Neo4j
```

---

## AI Integration (OpenCode)

Copy `AGENTS.md` to your home directory (or symlink) to enable autonomous KG updates:

```bash
ln -s /path/to/digital-twin/AGENTS.md ~/AGENTS.md
```

The AI agent reads `~/AGENTS.md` at session start and follows these rules:

| Trigger | Command |
|---------|---------|
| Software installed | `dt event --type SoftwareInstalled --entity-type Software ...` |
| Config changed | `dt event --type ConfigChange --entity-type NacosConfig ...` |
| Architecture decision | `dt memorize --type Decision --entity-type ArchitectureDecision ...` |
| Production deploy | `dt event --type Deploy --entity-type JenkinsJob ...` |
| Source file edited | `dt update --path <root> --name <project> --file <path>` |
| File deleted | `dt remove --project <name> --file <path>` |

---

## Supported Languages

| Language | File Extensions | Parser |
|----------|----------------|--------|
| Java | `.java` | tree-sitter-java |
| TypeScript | `.ts`, `.tsx` | tree-sitter-typescript |
| Python | `.py` | tree-sitter-python |
| Go | `.go` | tree-sitter-go |
| Rust | `.rs` | tree-sitter-rust |
| PHP | `.php` | tree-sitter-php |
| JavaScript | `.js`, `.jsx`, `.mjs`, `.cjs` | tree-sitter-javascript |

---

## Project Structure

```
digital-twin/
├── README.md
├── config.yaml                  # Central configuration
├── setup.sh                     # One-click deployment script
├── dt-sync                      # Orchestration script for incremental sync
│
├── engine-rust/                 # Rust CLI (dt)
│   ├── Cargo.toml
│   └── src/
│       ├── main.rs              # CLI entry point (clap)
│       ├── config.rs            # Config reader (YAML + env fallback)
│       ├── neo4j.rs             # Neo4j HTTP client
│       ├── qdrant.rs            # Qdrant HTTP client
│       ├── embed.rs             # Embed server HTTP client
│       ├── scanner.rs           # File scanner
│       ├── parser.rs            # tree-sitter parser
│       ├── models.rs            # Data models
│       ├── build.rs             # Index/build/update/validate logic
│       ├── event.rs             # Event node writer
│       ├── knowledge.rs         # Knowledge node writer
│       ├── remove.rs            # Code entity remover
│       └── search.rs            # Semantic search
│
├── services/
│   ├── embed-server/            # Embedding inference (Python)
│   │   ├── main.py
│   │   └── requirements.txt
│   └── search-web/              # Web search UI (Python)
│       ├── app.py
│       └── templates/
│
└── (data directories)           # SQLite cache, Neo4j data, etc.
```

---

## Configuration Reference

`config.yaml`:

| Key | Default | Description |
|-----|---------|-------------|
| `server.hostname` | `localhost` | Server hostname for inventory |
| `services.neo4j.url` | `http://localhost:7474` | Neo4j REST API URL |
| `services.neo4j.user` | `neo4j` | Neo4j username |
| `services.neo4j.password` | `neo4j` | Neo4j password |
| `services.qdrant.url` | `http://localhost:6333` | Qdrant REST API URL |
| `services.embed_server.url` | `http://localhost:8001` | Embed server URL |
| `services.embed_server.dim` | `768` | Embedding dimension |
| `services.embed_server.model` | `BAAI/bge-base-zh-v1.5` | Embedding model name |
| `snapshot_dir` | `/var/lib/digital-twin/snapshots` | Directory for snapshots |
| `projects` | `[]` | List of project definitions (name + path) |
| `watcher` | (internal) | File watcher config (for dt-sync) |

Environment variable `DT_CONFIG` overrides the config file path.

---

## Data Storage

| Data | Location | Technology |
|------|----------|------------|
| Knowledge graph | Neo4j (`localhost:7474`) | Nodes & relationships |
| Code vectors | Qdrant (`localhost:6333`) | Collections per project |
| File hash cache | `/var/lib/digital-twin/lazy.db` | SQLite |
| Embeddings | In-memory / CPU | BGE-base-zh-v1.5 |

---

## License

MIT
