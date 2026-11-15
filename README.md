# Recognitor

A local-first, high-performance Personal Knowledge Management (PKM) memory management system designed for LLMs and autonomous agents.

Recognitor is inspired by the simplicity of Markdown-based agent memory (such as Basic Memory), but addresses fundamental scaling, cross-domain relationship, and performance bottlenecks.

## Key Principles

- **Unified Graph, Logical Namespaces:** Store knowledge in a single coherent graph while maintaining clean domain boundaries (work, personal, research), with links that resolve across them.
- **Cross-Domain Traversal:** Connect concepts across different domains via typed relational edges queryable in a single engine.
- **File-First Truth:** Local Markdown files are canonical; the database is a high-speed, disposable projection.
- **Fast MCP & CLI:** Native Rust binary with in-process SQLite; no interpreter start-up.

## Documentation

- [Architecture Overview](docs/01-architecture-overview.md) - System vision, storage model, and tech stack comparison.
- [Open Questions & Tradeoffs](docs/02-open-questions-and-tradeoffs.md) - Design decisions, rejected alternatives, and remaining open questions.
- [Markdown Format & Edge Syntax](docs/03-markdown-format-and-edge-syntax.md) - Note anatomy, observations, relations, links, and link resolution.
- [Information Stores & Lifecycle](docs/04-information-stores-and-lifecycle.md) - Bootstrap, knowledge namespaces, archive, and session loading.
- [Database Schema](docs/05-database-schema-specification.md) - SQLite tables, indexes, query patterns, and database location.
- [Design Reviews](docs/06-design-reviews.md) - Review log and finding tracker.
- [Interface Contracts](docs/07-interface-contracts.md) - MCP tools and CLI commands: inputs, outputs, token budgets, and write-conflict rules.
