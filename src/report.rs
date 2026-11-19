// SPDX-License-Identifier: CPAL-1.0
// Copyright (c) 2026 Selçuk Öztürk.
// This file is subject to the Common Public Attribution License Version 1.0 (CPAL-1.0).
// See the LICENSE file in the repository root for full terms, including Exhibit A and Exhibit B.

//! Text output of `sync` and `rebuild`.
//!
//! The docs do not define this output (doc 02 §4 says only that `sync` "reports dirty files"), so the format here is
//! provisional. `--report` adds shadow-mode diagnostics that are not part of any contract.

use std::collections::BTreeMap;
use std::fmt::Write;

use crate::index::{FullCause, SweepKind, SweepReport};
use crate::parse::WarningKind;
use crate::walk::{NameError, NameErrorKind};

/// The text of a name the engine left out, without a severity prefix: `sync` and `rebuild` print it as a warning
/// (doc 07 §6), `lint` as an error.
pub fn name_error_text(e: &NameError) -> String {
    let what = match e.kind {
        NameErrorKind::NonAscii => "non-ASCII name, not indexed",
        NameErrorKind::CaseTwin => "case twins, none of them indexed",
    };
    format!("{what}: {} ({} notes left out)", e.paths.join(", "), e.notes_left_out)
}

/// Renders a sweep report. `verbose` adds every warning line and the diagnostics block.
pub fn render(r: &SweepReport, verbose: bool) -> String {
    let mut out = String::new();
    let t = &r.totals;
    let name = match r.kind {
        SweepKind::Rebuild => "rebuild",
        SweepKind::Sync => "sync",
    };
    let _ = writeln!(
        out,
        "{name}: {} notes, {} observations, {} edges ({} typed, {} inline, {} ghosts); read {} of {} files in {} ms",
        t.notes,
        t.observations,
        t.edges,
        t.typed_edges,
        t.edges - t.typed_edges,
        t.ghosts,
        r.files_read,
        r.files_seen,
        r.elapsed.as_millis()
    );
    match r.full {
        Some(FullCause::FreshDatabase) if r.kind == SweepKind::Sync => {
            let _ = writeln!(out, "new database: indexed the whole vault");
        }
        Some(FullCause::SchemaVersion(v)) => {
            let _ = writeln!(
                out,
                "schema version {v} -> {}: database rebuilt",
                crate::db::SCHEMA_VERSION
            );
        }
        _ => {}
    }
    if r.kind == SweepKind::Sync && r.full.is_none() {
        let _ = writeln!(
            out,
            concat!(
                "{} added, {} changed, {} deleted, {} retitled, {} renamed, {} unchanged after re-hash; ",
                "{} links re-resolved"
            ),
            r.added.len(),
            r.changed.len(),
            r.deleted.len(),
            r.retitled.len(),
            r.renamed.len(),
            r.rehashed,
            r.reresolved
        );
        for (label, ids) in [
            ("added", &r.added),
            ("changed", &r.changed),
            ("deleted", &r.deleted),
            ("retitled", &r.retitled),
            ("renamed", &r.renamed),
        ] {
            for id in ids {
                let _ = writeln!(out, "{label}: {id}");
            }
        }
    }
    for e in &r.name_errors {
        let _ = writeln!(out, "warning: {}", name_error_text(e));
    }
    if !r.warnings.is_empty() {
        let mut by_kind: BTreeMap<WarningKind, usize> = BTreeMap::new();
        for w in &r.warnings {
            *by_kind.entry(w.warning.kind).or_default() += 1;
        }
        let _ = writeln!(out, "warnings: {}", r.warnings.len());
        for (kind, n) in &by_kind {
            let _ = writeln!(out, "  {}: {n}", kind.label());
        }
    }
    if verbose {
        for w in &r.warnings {
            let line = w.warning.line.map(|l| format!(":{l}")).unwrap_or_default();
            let _ = writeln!(out, "warning: {}{line}: {}", w.note, w.warning.message);
        }
        if let Some(d) = &r.diagnostics {
            render_diagnostics(&mut out, d);
        }
    }
    out
}

fn render_diagnostics(out: &mut String, d: &crate::index::Diagnostics) {
    let bytes: i64 = d.namespaces.iter().map(|n| n.bytes).sum();
    let _ = writeln!(out, "\nreport (shadow mode, not a contract)");
    let _ = writeln!(out, "  database: {}", d.db_path);
    let _ = writeln!(
        out,
        "  markdown indexed: {} bytes ({:.0} KB)",
        bytes,
        bytes as f64 / 1024.0
    );
    let _ = writeln!(out, "  namespaces:");
    let width = d.namespaces.iter().map(|n| n.namespace.len()).max().unwrap_or(0);
    for n in &d.namespaces {
        let _ = writeln!(
            out,
            "    {:<width$}  notes {:>4}  observations {:>5}  edges {:>4}  ghosts {:>4}  bytes {:>7}  index.md {}",
            n.namespace,
            n.notes,
            n.observations,
            n.edges,
            n.ghosts,
            n.bytes,
            if n.has_index { "yes" } else { "no" }
        );
    }
    if !d
        .namespaces
        .iter()
        .any(|n| n.namespace == crate::paths::BOOTSTRAP_NAMESPACE)
    {
        let _ = writeln!(out, "  bootstrap: no notes in the bootstrap namespace");
    }
    let _ = writeln!(out, "  link resolution (links resolved in this run):");
    for (step, n) in &d.steps {
        let _ = writeln!(out, "    {}: {n}", step.label());
    }
    let _ = writeln!(out, "    ghost, no match: {}", d.ghost_links);
    let _ = writeln!(out, "    ghost, ambiguous: {}", d.ambiguous_links);
    let _ = writeln!(out, "  top ghost targets (doc 05 §3.5, grouped by ref_key):");
    for g in &d.top_ghosts {
        let _ = writeln!(out, "    {:>3}  {}  ({})", g.mentions, g.target_ref, g.predicates);
    }
    let _ = writeln!(
        out,
        "  suppressed by namespace rules: archive observations {}, archive links {}, bootstrap observations {}",
        d.suppressed_archive_observations, d.suppressed_archive_links, d.suppressed_bootstrap_observations
    );
    let _ = writeln!(
        out,
        "  links merged into an existing row (same predicate and target): {}",
        d.collapsed_links
    );
    let _ = writeln!(out, "  bracket-led list items that are not observations:");
    if d.near_misses.is_empty() {
        let _ = writeln!(out, "    none");
    }
    for (miss, n) in &d.near_misses {
        let _ = writeln!(out, "    {}: {n}", miss.label());
    }
    let _ = writeln!(
        out,
        "  spec gaps (placeholder behaviour fired, notes parsed in this run):"
    );
    if d.gaps.is_empty() {
        let _ = writeln!(out, "    none");
    }
    for (gap, n) in &d.gaps {
        let _ = writeln!(out, "    {}: {n}", gap.label());
    }
    let w = &d.walk;
    let hidden_md = w
        .hidden_md_files
        .map(|n| format!(" ({n} .md files inside)"))
        .unwrap_or_default();
    let _ = writeln!(
        out,
        "  walk: hidden folders skipped: {}{hidden_md}",
        list_or_none(&w.hidden_dirs)
    );
    let others: Vec<String> = w
        .other_files
        .iter()
        .map(|(ext, n)| format!("{} {n}", if ext.is_empty() { "(none)" } else { ext }))
        .collect();
    let _ = writeln!(out, "  walk: other files not indexed: {}", list_or_none(&others));
    let _ = writeln!(
        out,
        "  walk: .md files in the vault root (not notes): {}, symlinks {}",
        w.root_md_files, w.symlinks
    );
}

fn list_or_none(items: &[String]) -> String {
    if items.is_empty() {
        "none".to_string()
    } else {
        items.join(", ")
    }
}
