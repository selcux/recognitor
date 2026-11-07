# Recognitor

A local-first, high-performance Personal Knowledge Management (PKM) memory management system designed for LLMs and autonomous agents.

Recognitor is inspired by the simplicity of Markdown-based agent memory (such as Basic Memory), but addresses fundamental scaling, cross-domain relationship, and performance bottlenecks.

## Key Principles

- **Unified Graph, Logical Namespaces:** Store knowledge in a single coherent graph while maintaining clean domain boundaries (work, personal, research) without database silos.
- **Cross-Domain Traversal:** Connect concepts across different domains via typed relational edges queryable in a single engine.
- **File-First Truth:** Local Markdown files are canonical; the database is a high-speed, disposable projection.
- **Instant MCP & CLI Latency:** Designed for lightweight execution and sub-millisecond responsiveness.

## Documentation

- [Architecture Overview](docs/01-architecture-overview.md) - System vision, storage model, and tech stack comparison.
- [Open Questions & Tradeoffs](docs/02-open-questions-and-tradeoffs.md) - Unresolved design decisions, edge notations, and sync strategies.
