//! Deterministic streaming generator for the 100x Target Scale Benchmark vault (doc 02 §11).
//!
//! Generates 10,000 notes, ~100,000 observations, ~20,000 links, ~25 MB Markdown
//! with peak memory usage under 5 MB RSS via a two-phase compact plan and streaming write.
//!
//! usage: cargo run --release --example gen_scale_vault -- <output-dir> [--seed <u64>] [--embed-vault <path>]

use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

type Res<T> = Result<T, Box<dyn std::error::Error>>;

const TOTAL_NOTES: usize = 10_000;
const BOOTSTRAP_NOTES: usize = 14;
const KNOWLEDGE_NOTES: usize = 8_500;
const NAMESPACE_COUNT: usize = 20;
const TARGET_OBSERVATIONS: usize = 100_000;
const TARGET_EDGES: usize = 20_000;

/// Fast, deterministic 64-bit PRNG (SplitMix64).
struct Rng {
    state: u64,
}

impl Rng {
    fn new(seed: u64) -> Self {
        Self {
            state: seed.wrapping_add(0x9e3779b97f4a7c15),
        }
    }

    fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9e3779b97f4a7c15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
        z ^ (z >> 31)
    }

    fn gen_range(&mut self, low: usize, high: usize) -> usize {
        assert!(low < high);
        low + (self.next_u64() as usize % (high - low))
    }

    fn gen_ratio(&mut self, numerator: usize, denominator: usize) -> bool {
        self.gen_range(0, denominator) < numerator
    }

    fn choose<'a, T>(&mut self, slice: &'a [T]) -> &'a T {
        &slice[self.gen_range(0, slice.len())]
    }
}

// Vocabulary tiers for realistic Zipf-like FTS5 distribution.
const TIER1_WORDS: &[&str] = &[
    "system",
    "state",
    "model",
    "component",
    "process",
    "layer",
    "interface",
    "module",
    "service",
    "event",
    "resource",
    "protocol",
    "context",
    "handler",
    "manager",
];

const TIER2_WORDS: &[&str] = &[
    "cache",
    "latency",
    "throughput",
    "pipeline",
    "buffer",
    "transaction",
    "storage",
    "worker",
    "runtime",
    "scheduler",
    "dispatch",
    "cluster",
    "replica",
    "snapshot",
    "checksum",
    "parser",
    "compiler",
    "optimizer",
    "backoff",
    "resilience",
    "concurrency",
    "telemetry",
    "metric",
    "allocator",
    "serialization",
    "iterator",
    "stream",
    "mutex",
    "registry",
    "payload",
];

const TIER3_TOPICS: &[&str] = &[
    "raft_consensus",
    "wal_replay",
    "btree_split",
    "paxos_lease",
    "bloom_filter",
    "tokio_epoll",
    "sqlite_fts5",
    "simd_avx2",
    "zero_copy_mmap",
    "page_rank_power",
    "hyperloglog_cardinality",
    "tcmalloc_arena",
    "quic_congestion",
    "ebpf_filter",
    "skip_list_index",
    "consistent_hashing",
    "copy_on_write",
    "read_copy_update",
    "lock_free_queue",
    "vector_quantization",
    "inverted_index",
    "two_phase_commit",
    "gossip_membership",
    "merkle_dag",
    "lsm_compaction",
];

const TIER4_RARE: &[&str] = &[
    "rare_needle_alpha_472",
    "rare_gem_beta_918",
    "anomalous_trace_zeta_104",
    "exotic_quark_delta_501",
    "singular_matrix_omega_883",
    "hidden_beacon_theta_319",
    "quantum_vortex_kappa_762",
    "isolated_pulsar_lambda_640",
];

const QUALIFIERS: &[&str] = &[
    "@valid:2026",
    "@valid:2026-11",
    "@occurred:2026-05-12",
    "@effective:2026-01-01",
    "@due:2026-12-31",
    "@mentioned:2026-08",
];

const REL_PREDICATES: &[&str] = &[
    "indexes",
    "relates_to",
    "has_part",
    "part_of",
    "supersedes",
    "depends_on",
    "implements",
    "documents",
];

const OBS_CATEGORIES: &[&str] = &[
    "fact",
    "rule",
    "decision",
    "goal",
    "preference",
    "open",
    "works",
    "fails",
    "avoid",
];

const OBS_TAGS: &[&str] = &[
    "#architecture",
    "#perf",
    "#storage",
    "#concurrency",
    "#search",
    "#cache",
    "#memory",
    "#network",
    "#security",
    "#database",
    "#resilience",
    "#telemetry",
];

#[derive(Clone, Copy)]
enum Store {
    Bootstrap,
    Knowledge,
    Archive,
}

#[derive(Clone, Copy)]
enum NodeRole {
    SuperHub,
    MidHub,
    Leaf,
}

struct NodePlan {
    id: u32,
    store: Store,
    ns_idx: u8,
    subdir_idx: u8,
    note_idx_in_dir: u16,
    role: NodeRole,
    target_bytes: u32,
    obs_count: u16,
}

#[derive(Clone, Copy)]
struct EdgePlan {
    src: u32,
    dst: u32, // target node id, or u32::MAX for ghost
    is_inline: bool,
    predicate_idx: u8,
}

struct Args {
    out_dir: PathBuf,
    seed: u64,
    embed_vault: Option<PathBuf>,
}

fn parse_args() -> Res<Args> {
    let mut positional = Vec::new();
    let mut seed = 42u64;
    let mut embed_vault = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--seed" => seed = args.next().ok_or("--seed needs a number")?.parse()?,
            "--embed-vault" => embed_vault = Some(PathBuf::from(args.next().ok_or("--embed-vault needs a path")?)),
            _ => positional.push(PathBuf::from(arg)),
        }
    }
    if positional.is_empty() {
        return Err("usage: gen_scale_vault <output-dir> [--seed <u64>] [--embed-vault <path>]".into());
    }
    Ok(Args {
        out_dir: positional.remove(0),
        seed,
        embed_vault,
    })
}

/// Computes relative path inside vault for a node ID.
fn relative_path(plan: &NodePlan) -> PathBuf {
    match plan.store {
        Store::Bootstrap => {
            if plan.id == 0 {
                PathBuf::from("bootstrap/index.md")
            } else if plan.id < 10 {
                PathBuf::from(format!("bootstrap/rule_{:02}.md", plan.id))
            } else {
                PathBuf::from(format!("bootstrap/persona_{:02}.md", plan.id))
            }
        }
        Store::Knowledge => {
            let ns = format!("ns{:02}", plan.ns_idx);
            if plan.note_idx_in_dir == 0 && plan.subdir_idx == 0 {
                PathBuf::from(format!("{ns}/index.md"))
            } else {
                let sub = match plan.subdir_idx {
                    0 => "sub0".to_string(),
                    1 => "sub1/deep0".to_string(),
                    _ => format!("sub{}", plan.subdir_idx),
                };
                PathBuf::from(format!("{ns}/{sub}/k_{:04}.md", plan.id))
            }
        }
        Store::Archive => {
            let ns = format!("ns{:02}", plan.ns_idx);
            let sub = match plan.subdir_idx {
                0 => "sub0".to_string(),
                1 => "sub1/deep0".to_string(),
                _ => format!("sub{}", plan.subdir_idx),
            };
            PathBuf::from(format!("archive/{ns}/{sub}/arch_{:04}.md", plan.id))
        }
    }
}

/// Returns the slug/name of a node for wikilinking.
fn wikilink_target(plan: &NodePlan) -> String {
    match plan.store {
        Store::Bootstrap => {
            if plan.id == 0 {
                "bootstrap::index".to_string()
            } else if plan.id < 10 {
                format!("rule_{:02}", plan.id)
            } else {
                format!("persona_{:02}", plan.id)
            }
        }
        Store::Knowledge => {
            if plan.note_idx_in_dir == 0 && plan.subdir_idx == 0 {
                format!("ns{:02}::index", plan.ns_idx)
            } else {
                format!("k_{:04}", plan.id)
            }
        }
        Store::Archive => format!("arch_{:04}", plan.id),
    }
}

fn build_topology_plan(rng: &mut Rng) -> (Vec<NodePlan>, Vec<EdgePlan>) {
    let mut nodes = Vec::with_capacity(TOTAL_NOTES);

    // 1. Bootstrap notes (IDs 0..13)
    for id in 0..BOOTSTRAP_NOTES {
        nodes.push(NodePlan {
            id: id as u32,
            store: Store::Bootstrap,
            ns_idx: 0,
            subdir_idx: 0,
            note_idx_in_dir: id as u16,
            role: NodeRole::Leaf,
            target_bytes: 800,
            obs_count: 0, // Bootstrap notes store zero observations in db
        });
    }

    // 2. Knowledge notes (IDs 14..8513)
    // 20 namespaces, 425 notes each = 8,500 notes.
    // 25 super-hubs: 20 namespace index.md notes + 5 MOC notes.
    let mut total_obs_assigned = 0;
    let k_start = BOOTSTRAP_NOTES;
    let k_end = k_start + KNOWLEDGE_NOTES;

    for id in k_start..k_end {
        let k_idx = id - k_start;
        let ns_idx = (k_idx / (KNOWLEDGE_NOTES / NAMESPACE_COUNT)) as u8;
        let note_in_ns = k_idx % (KNOWLEDGE_NOTES / NAMESPACE_COUNT);
        let subdir_idx = (note_in_ns % 4) as u8; // 4 subdirs: sub0, sub1/deep0, sub2, sub3
        let note_in_dir = (note_in_ns / 4) as u16;

        let is_ns_index = note_in_ns == 0 && subdir_idx == 0;
        let is_moc = (1..6).contains(&k_idx); // 5 MOC notes

        let role = if is_ns_index || is_moc {
            NodeRole::SuperHub
        } else if rng.gen_ratio(1, 10) {
            NodeRole::MidHub
        } else {
            NodeRole::Leaf
        };

        // Size: 99% normal (1KB - 4KB), 1% oversized (25KB - 45KB)
        let is_oversized = rng.gen_ratio(1, 100);
        let target_bytes = if is_oversized {
            rng.gen_range(25_000, 45_000) as u32
        } else {
            rng.gen_range(1_200, 3_800) as u32
        };

        // Observations: target ~11.76 avg across 8,500 notes to reach 100,000
        let obs = if id == k_end - 1 {
            TARGET_OBSERVATIONS.saturating_sub(total_obs_assigned) as u16
        } else {
            rng.gen_range(6, 18) as u16
        };
        total_obs_assigned += obs as usize;

        nodes.push(NodePlan {
            id: id as u32,
            store: Store::Knowledge,
            ns_idx,
            subdir_idx,
            note_idx_in_dir: note_in_dir,
            role,
            target_bytes,
            obs_count: obs,
        });
    }

    // 3. Archive notes (IDs 8514..9999) -> 1,486 notes
    let arch_start = k_end;
    for id in arch_start..TOTAL_NOTES {
        let arch_idx = id - arch_start;
        let ns_idx = (arch_idx % NAMESPACE_COUNT) as u8;
        let subdir_idx = ((arch_idx / NAMESPACE_COUNT) % 3) as u8;
        let note_in_dir = (arch_idx / (NAMESPACE_COUNT * 3)) as u16;

        let is_oversized = rng.gen_ratio(1, 100);
        let target_bytes = if is_oversized {
            rng.gen_range(20_000, 35_000) as u32
        } else {
            rng.gen_range(1_500, 3_500) as u32
        };

        nodes.push(NodePlan {
            id: id as u32,
            store: Store::Archive,
            ns_idx,
            subdir_idx,
            note_idx_in_dir: note_in_dir,
            role: NodeRole::Leaf,
            target_bytes,
            obs_count: 0, // Archive notes produce 0 observations in recognitor
        });
    }

    // 4. Edges Plan
    let mut edges = Vec::with_capacity(TARGET_EDGES + 100);

    // 4a. Bootstrap links: index.md -> rule_01..rule_04 -> rule_05..rule_09 (tree hierarchy)
    for target in 1..=4 {
        edges.push(EdgePlan {
            src: 0,
            dst: target as u32,
            is_inline: false,
            predicate_idx: 0, // indexes
        });
    }
    for target in 5..=9 {
        let parent = (target % 4) + 1;
        edges.push(EdgePlan {
            src: parent as u32,
            dst: target as u32,
            is_inline: false,
            predicate_idx: 2, // has_part
        });
    }

    // 4b. Planted convergent targets: 25 selected knowledge notes that receive 15-30 incoming links
    let convergent_targets: Vec<u32> = (k_start..k_start + 25).map(|x| x as u32).collect();

    // 4c. Generate knowledge edges across knowledge notes
    let mut edges_created = 0;
    let target_k_edges = TARGET_EDGES - edges.len();

    // Edge assignment per node:
    // SuperHub: 150-300 edges (at least two with 260+)
    // MidHub: 6-15 edges
    // Leaf: 0-2 edges
    for plan in &nodes[k_start..k_end] {
        let quota = match plan.role {
            NodeRole::SuperHub => {
                if plan.id == k_start as u32 || plan.id == (k_start + 1) as u32 {
                    rng.gen_range(260, 300)
                } else {
                    rng.gen_range(140, 220)
                }
            }
            NodeRole::MidHub => rng.gen_range(6, 15),
            NodeRole::Leaf => {
                if edges_created < target_k_edges && rng.gen_ratio(6, 10) {
                    rng.gen_range(1, 3)
                } else {
                    0
                }
            }
        };

        let mut seen_pairs = HashSet::new();

        for _ in 0..quota {
            if edges_created >= target_k_edges {
                break;
            }

            let is_inline = rng.gen_ratio(45, 100); // 45% inline, 55% explicit ## Relations
            let predicate_idx = if is_inline {
                0
            } else {
                (rng.gen_range(0, REL_PREDICATES.len())) as u8
            };

            // 3% ghost link
            let is_ghost = rng.gen_ratio(3, 100);
            let dst_id = if is_ghost {
                u32::MAX
            } else if rng.gen_ratio(5, 100) {
                // Link to one of the planted convergent targets
                *rng.choose(&convergent_targets)
            } else if rng.gen_ratio(70, 100) {
                // Link inside same namespace
                let ns_base = k_start + (plan.ns_idx as usize) * (KNOWLEDGE_NOTES / NAMESPACE_COUNT);
                let ns_limit = ns_base + (KNOWLEDGE_NOTES / NAMESPACE_COUNT);
                rng.gen_range(ns_base, ns_limit) as u32
            } else {
                // Cross-namespace link
                rng.gen_range(k_start, k_end) as u32
            };

            if dst_id == plan.id || !seen_pairs.insert((is_inline, predicate_idx, dst_id)) {
                continue;
            }

            edges.push(EdgePlan {
                src: plan.id,
                dst: dst_id,
                is_inline,
                predicate_idx,
            });
            edges_created += 1;
        }
    }

    (nodes, edges)
}

fn stream_write_vault(out_dir: &Path, nodes: &[NodePlan], edges: &[EdgePlan], rng: &mut Rng) -> Res<()> {
    // Group outgoing edges by source node using CSR index
    let mut edge_offsets = vec![0usize; TOTAL_NOTES + 1];
    for edge in edges {
        edge_offsets[edge.src as usize + 1] += 1;
    }
    for i in 0..TOTAL_NOTES {
        edge_offsets[i + 1] += edge_offsets[i];
    }

    let mut edge_pos = edge_offsets.clone();
    let mut sorted_edges = vec![
        EdgePlan {
            src: 0,
            dst: 0,
            is_inline: false,
            predicate_idx: 0,
        };
        edges.len()
    ];
    for edge in edges {
        let pos = &mut edge_pos[edge.src as usize];
        sorted_edges[*pos] = *edge;
        *pos += 1;
    }

    // Single reused string buffer for formatting each note
    let mut buf = String::with_capacity(64 * 1024);

    for plan in nodes {
        buf.clear();
        let path = out_dir.join(relative_path(plan));
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        let node_edges = &sorted_edges[edge_offsets[plan.id as usize]..edge_offsets[plan.id as usize + 1]];

        match plan.store {
            Store::Bootstrap => {
                buf.push_str("---\n");
                buf.push_str(&format!("title: Bootstrap Node {:02}\n", plan.id));
                buf.push_str("type: rule\n");
                buf.push_str("---\n\n");
                buf.push_str(&format!("# Bootstrap Node {:02}\n\n", plan.id));
                buf.push_str("Standing instruction and operational guideline.\n\n");
                buf.push_str("Ensure deterministic behavior, bounded tokens, and strict schema conformance.\n\n");

                if !node_edges.is_empty() {
                    buf.push_str("## Relations\n");
                    for edge in node_edges {
                        let pred = REL_PREDICATES[edge.predicate_idx as usize];
                        let target_str = wikilink_target(&nodes[edge.dst as usize]);
                        buf.push_str(&format!("- {pred} [[{target_str}]]\n"));
                    }
                    buf.push('\n');
                }
            }
            Store::Knowledge | Store::Archive => {
                let is_archive = matches!(plan.store, Store::Archive);
                let title = if plan.note_idx_in_dir == 0 && plan.subdir_idx == 0 && !is_archive {
                    format!("Namespace {:02} Index", plan.ns_idx)
                } else if is_archive {
                    format!("Archive Record {:04}", plan.id)
                } else {
                    format!("Knowledge Note {:04}", plan.id)
                };

                // Frontmatter
                buf.push_str("---\n");
                buf.push_str(&format!("title: {title}\n"));
                buf.push_str("type: concept\n");
                buf.push_str("tags:\n");
                buf.push_str(&format!("- ns{:02}\n", plan.ns_idx));
                buf.push_str("- benchmark\n");
                buf.push_str("---\n\n");

                // Title H1
                buf.push_str(&format!("# {title}\n\n"));

                // Summary paragraph (fits SUMMARY_MAX_CHARS)
                let t1_word = *rng.choose(TIER1_WORDS);
                let t2_word = *rng.choose(TIER2_WORDS);
                let t3_word = *rng.choose(TIER3_TOPICS);
                buf.push_str(&format!(
                    "Core architecture specification describing {t1_word} implementation, \
                    managing {t2_word} invariants under {t3_word} operation.\n\n"
                ));

                // Body paragraphs with vocabulary and inline links
                let inline_edges: Vec<&EdgePlan> = node_edges.iter().filter(|e| e.is_inline).collect();
                let explicit_edges: Vec<&EdgePlan> = node_edges.iter().filter(|e| !e.is_inline).collect();

                let mut current_bytes = buf.len();
                let mut p_idx = 0;
                while current_bytes < (plan.target_bytes as usize).saturating_sub(600) {
                    let w1 = *rng.choose(TIER1_WORDS);
                    let w2 = *rng.choose(TIER2_WORDS);
                    let w3 = *rng.choose(TIER3_TOPICS);
                    let w4 = *rng.choose(TIER4_RARE);

                    buf.push_str(&format!(
                        "Detailed paragraph {p_idx} outlining subsystem behavior: {w1} orchestrates \
                        robust {w2} streams. Performance metrics confirm {w3} stability. Special audit trace: {w4}. "
                    ));

                    // Embed inline link if available (emit each inline edge at most once)
                    if p_idx < inline_edges.len() {
                        let edge = inline_edges[p_idx];
                        if edge.dst == u32::MAX {
                            buf.push_str(&format!(
                                "Reference ghost item [[ghost_node_{:04}]]. ",
                                rng.gen_range(1, 9999)
                            ));
                        } else {
                            let target_str = wikilink_target(&nodes[edge.dst as usize]);
                            buf.push_str(&format!("Interacts with component [[{target_str}]]. "));
                        }
                    }

                    buf.push_str(
                        "The component evaluates state transitions synchronously and enforces non-blocking \
                        memory boundaries across execution phases.\n\n",
                    );
                    p_idx += 1;
                    current_bytes = buf.len();
                }

                // Any remaining inline edges emitted in a supplementary paragraph
                if p_idx < inline_edges.len() {
                    buf.push_str("Supplementary references: ");
                    while p_idx < inline_edges.len() {
                        let edge = inline_edges[p_idx];
                        if edge.dst == u32::MAX {
                            buf.push_str(&format!("probe [[ghost_node_{:04}]] ", rng.gen_range(1, 9999)));
                        } else {
                            let target_str = wikilink_target(&nodes[edge.dst as usize]);
                            buf.push_str(&format!("related [[{target_str}]] "));
                        }
                        p_idx += 1;
                    }
                    buf.push_str("documented for completeness.\n\n");
                }

                // Observations section
                if plan.obs_count > 0 {
                    buf.push_str("## Observations\n");
                    for _ in 0..plan.obs_count {
                        let cat = *rng.choose(OBS_CATEGORIES);
                        let qual = *rng.choose(QUALIFIERS);
                        let tag1 = *rng.choose(OBS_TAGS);
                        let tag2 = *rng.choose(OBS_TAGS);
                        let w1 = *rng.choose(TIER1_WORDS);
                        let w2 = *rng.choose(TIER2_WORDS);
                        let w3 = *rng.choose(TIER3_TOPICS);

                        buf.push_str(&format!(
                            "- [{cat}] {qual} {w1} maintains {w2} guarantees during {w3} evaluation. {tag1} {tag2}\n"
                        ));
                    }
                    buf.push('\n');
                }

                // Relations section
                if !explicit_edges.is_empty() {
                    buf.push_str("## Relations\n");
                    for edge in explicit_edges {
                        let pred = REL_PREDICATES[edge.predicate_idx as usize];
                        if edge.dst == u32::MAX {
                            buf.push_str(&format!(
                                "- {pred} [[ghost_node_{:04}]] (unresolved probe)\n",
                                rng.gen_range(1, 9999)
                            ));
                        } else {
                            let target_str = wikilink_target(&nodes[edge.dst as usize]);
                            buf.push_str(&format!("- {pred} [[{target_str}]] (structural link)\n"));
                        }
                    }
                    buf.push('\n');
                }
            }
        }

        let file = File::create(&path)?;
        let mut writer = BufWriter::new(file);
        writer.write_all(buf.as_bytes())?;
        writer.flush()?;
    }

    Ok(())
}

fn copy_dir_all(src: &Path, dst: &Path) -> Res<usize> {
    let mut count = 0;
    if !src.exists() {
        return Ok(0);
    }
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let dst_path = dst.join(entry.file_name());
        if ty.is_dir() {
            fs::create_dir_all(&dst_path)?;
            count += copy_dir_all(&entry.path(), &dst_path)?;
        } else if entry.path().extension().and_then(|s| s.to_str()) == Some("md") {
            fs::copy(entry.path(), dst_path)?;
            count += 1;
        }
    }
    Ok(count)
}

fn main() -> Res<()> {
    let args = parse_args()?;
    println!("=== 100x Scale Vault Generator (doc 02 §11) ===");
    println!("Output directory: {}", args.out_dir.display());
    println!("PRNG seed:        {}", args.seed);

    if args.out_dir.exists() {
        println!("Cleaning existing output directory...");
        fs::remove_dir_all(&args.out_dir)?;
    }
    fs::create_dir_all(&args.out_dir)?;

    let t0 = Instant::now();
    let mut rng = Rng::new(args.seed);

    println!("Phase 1: Planning compact topology (10,000 nodes, 20,000 edges)...");
    let (nodes, edges) = build_topology_plan(&mut rng);
    let t_plan = t0.elapsed();
    println!(
        "Topology planned in {:>7.2} ms ({} nodes, {} edges planned)",
        t_plan.as_secs_f64() * 1000.0,
        nodes.len(),
        edges.len()
    );

    println!("Phase 2: Streaming Markdown generation to disk...");
    let t_write_start = Instant::now();
    stream_write_vault(&args.out_dir, &nodes, &edges, &mut rng)?;
    let t_write = t_write_start.elapsed();
    println!("Markdown generated in {:>7.2} s", t_write.as_secs_f64());

    if let Some(ref embed_path) = args.embed_vault {
        println!("Embedding reference vault from {}...", embed_path.display());
        let embed_dest = args.out_dir.join("reference_vault");
        fs::create_dir_all(&embed_dest)?;
        let count = copy_dir_all(embed_path, &embed_dest)?;
        println!("Embedded {count} reference notes into reference_vault/");
    }

    // Calculate total disk size
    let mut total_bytes = 0u64;
    let mut file_count = 0usize;
    for entry in walkdir(args.out_dir.as_path())? {
        if let Ok(meta) = entry.metadata() {
            if meta.is_file() {
                file_count += 1;
                total_bytes += meta.len();
            }
        }
    }

    println!("\nGeneration complete in {:>7.2} s:", t0.elapsed().as_secs_f64());
    println!("  Total files:       {file_count}");
    println!(
        "  Total disk size:   {:.2} MB ({total_bytes} bytes)",
        total_bytes as f64 / (1024.0 * 1024.0)
    );
    println!(
        "  Average file size: {:.1} KB",
        (total_bytes as f64 / file_count as f64) / 1024.0
    );

    Ok(())
}

fn walkdir(dir: &Path) -> Res<Vec<fs::DirEntry>> {
    let mut out = Vec::new();
    if dir.is_dir() {
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                out.extend(walkdir(&path)?);
            } else {
                out.push(entry);
            }
        }
    }
    Ok(out)
}
