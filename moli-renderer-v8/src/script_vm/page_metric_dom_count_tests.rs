//! The per-turn page metric snapshot reports the node and frame counts of the
//! full Moli DOM memory scan without paying for that scan on every turn.

use std::time::{Duration, Instant};

use super::ScriptVm;
use super::dom_heavy_regression_tests::new_dom_regression_vm;
use super::moli_dom_memory_counters;
use crate::runtime::{
    PageId, RendererDocumentLifecycleSnapshot, RendererDocumentToken, RendererFrameToken,
    RendererLifecycleEpoch, RendererLifecycleEventStamp,
};

fn lifecycle_snapshot() -> RendererDocumentLifecycleSnapshot {
    let page_id = PageId::new_for_testing(1);
    RendererDocumentLifecycleSnapshot {
        frame: RendererFrameToken { page_id },
        document: RendererDocumentToken::new_for_testing(page_id, 1),
        epoch: RendererLifecycleEpoch(0),
        started: RendererLifecycleEventStamp {
            sequence: 0,
            timestamp_micros: 0,
        },
        dom_content_loaded: None,
        load: None,
        terminated: None,
    }
}

/// Returns the full-scan iframe/frame element count after checking that the
/// snapshot agrees with the full scan.
fn assert_snapshot_matches_full_scan(vm: &ScriptVm, step: &str) -> usize {
    let snapshot = vm.performance_metric_snapshot_without_script(lifecycle_snapshot(), 0);
    let full_scan = moli_dom_memory_counters(vm.document_runtime.dom_host().dom());
    assert_eq!(
        snapshot.node_count,
        Some(full_scan.node_count as f64),
        "nodeCount after {step}"
    );
    assert_eq!(
        snapshot.frame_count,
        Some((1 + full_scan.iframe_element_count) as f64),
        "frameCount after {step}"
    );
    full_scan.iframe_element_count
}

#[test]
fn page_metric_snapshot_counts_match_the_full_dom_scan_across_mutations() {
    let mut vm = new_dom_regression_vm();
    let steps = [
        (
            "document scaffolding",
            r#"
            if (!document.documentElement) {
                document.appendChild(document.createElement('html'));
            }
            if (!document.body) {
                document.documentElement.appendChild(document.createElement('body'));
            }
            "#,
        ),
        (
            "frame element inserts",
            r#"
            for (const name of ['iframe', 'iframe', 'frame', 'div']) {
                document.body.appendChild(document.createElement(name));
            }
            document.body.appendChild(
                document.createElementNS('http://www.w3.org/1999/xhtml', 'iframe'));
            document.body.appendChild(
                document.createElementNS('http://www.w3.org/2000/svg', 'iframe'));
            document.createElement('iframe');
            "#,
        ),
        (
            "frame element removes",
            r#"
            for (const frame of Array.from(document.querySelectorAll('iframe, frame'))) {
                frame.remove();
            }
            document.body.lastChild.remove();
            "#,
        ),
        (
            "innerHTML replacement",
            r#"
            document.body.innerHTML =
                '<div><iframe></iframe><p>text</p></div><template><iframe></iframe></template>';
            document.body.innerHTML = '<section><iframe></iframe></section><!-- c -->';
            document.body.innerHTML = '';
            "#,
        ),
        (
            "cloning and detached documents",
            r#"
            document.body.innerHTML = '<iframe></iframe>';
            document.body.appendChild(document.body.cloneNode(true));
            const other = document.implementation.createHTMLDocument('other');
            other.body.innerHTML = '<iframe></iframe>';
            document.body.appendChild(document.importNode(other.body, true));
            "#,
        ),
    ];

    let mut frame_elements = assert_snapshot_matches_full_scan(&vm, "bootstrap");
    for (step, script) in steps {
        vm.eval(&format!("(() => {{ {script} return 'ok'; }})()"))
            .unwrap_or_else(|error| panic!("{step} should evaluate: {error:?}"));
        frame_elements = assert_snapshot_matches_full_scan(&vm, step);
    }
    assert!(
        frame_elements >= 9,
        "the mutations should leave frame elements in the arena, found {frame_elements}"
    );
}

fn fastest_snapshot_batch(vm: &ScriptVm) -> Duration {
    const BATCH: usize = 20;
    (0..5)
        .map(|_| {
            let started = Instant::now();
            for _ in 0..BATCH {
                std::hint::black_box(
                    vm.performance_metric_snapshot_without_script(lifecycle_snapshot(), 0),
                );
            }
            started.elapsed()
        })
        .min()
        .expect("at least one batch")
}

fn create_detached_spans(vm: &mut ScriptVm, count: usize) {
    vm.eval(&format!(
        "(() => {{ for (let i = 0; i < {count}; i++) document.createElement('span'); return 'ok'; }})()"
    ))
    .expect("span creation should evaluate");
}

#[test]
fn page_metric_snapshot_cost_does_not_scale_with_arena_node_count() {
    let mut vm = new_dom_regression_vm();
    create_detached_spans(&mut vm, 1_000);
    let small = fastest_snapshot_batch(&vm);
    create_detached_spans(&mut vm, 99_000);
    assert!(vm.document_runtime.dom_host().dom().len() >= 100_000);
    let large = fastest_snapshot_batch(&vm);

    // A per-node scan makes the 100x larger arena about 100x slower; the
    // absolute slack absorbs scheduler noise around sub-millisecond batches.
    assert!(
        large < small * 10 + Duration::from_millis(5),
        "snapshot batch took {large:?} at 100k nodes against {small:?} at 1k nodes"
    );
}
