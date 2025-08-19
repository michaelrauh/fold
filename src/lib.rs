pub mod error;
pub mod interner;
pub mod ortho;
pub mod ortho_database;
pub mod queue;
pub mod spatial;
pub mod splitter;

pub use error::*;
pub use interner::*;
pub use ortho_database::*;
pub use queue::*;
use tracing::instrument;
use opentelemetry::{KeyValue};
use opentelemetry_sdk::{Resource, trace as sdktrace};
use opentelemetry_otlp::{Protocol, WithExportConfig};
use tracing_subscriber::prelude::__tracing_subscriber_SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use opentelemetry::trace::TracerProvider;
use tracing_subscriber::EnvFilter;
use once_cell::sync::Lazy;
use std::sync::Mutex;
use lru::LruCache;

pub fn init_tracing(service_name: &str) {
    let mut builder = EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        // Default filter: info for our crate, warn for noisy deps
        EnvFilter::new("info,aws_smithy_runtime=warn,aws_smithy_runtime_api=warn,hyper=warn,aws_config=warn,aws_smithy_http=warn")
    });
    if std::env::var("FOLD_LOG_VERBOSE").is_ok() {
        builder = EnvFilter::new("debug");
    }
    // Allow overriding the OTLP endpoint via env so developers can point to a local
    // collector or the in-cluster Jaeger collector. Default to the jaeger-collector
    // service in the default namespace which the Jaeger Helm chart installs.
    let default_otlp_endpoint = "http://jaeger-collector.default.svc.cluster.local:4318/v1/traces".to_string();
    let otlp_endpoint = std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT").unwrap_or(default_otlp_endpoint);
    let otlp_exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_http()
        .with_protocol(Protocol::HttpBinary)
        .with_endpoint(&otlp_endpoint)
        .build()
        .expect("Failed to build OTLP exporter");
    let resource = Resource::builder_empty()
        .with_attributes(vec![KeyValue::new("service.name", service_name.to_string())])
        .build();
    let tracer_provider = sdktrace::SdkTracerProvider::builder()
        .with_simple_exporter(otlp_exporter)
        .with_resource(resource)
        .build();
    let tracer = tracer_provider.tracer(service_name.to_string());
    let otel_layer = tracing_opentelemetry::layer().with_tracer(tracer);
    tracing_subscriber::registry()
        .with(builder)
        .with(otel_layer)
        .with(tracing_subscriber::fmt::layer())
        .init();
}

pub struct Follower {
    low_version: Option<usize>,
    high_version: Option<usize>,
    low_interner: Option<crate::interner::Interner>,
    high_interner: Option<crate::interner::Interner>,
}

impl Follower {
    pub fn new() -> Self {
        Follower {
            low_version: None,
            high_version: None,
            low_interner: None,
            high_interner: None,
        }
    }

    #[instrument(skip_all)]
    pub fn run_follower_once<Q: crate::queue::QueueProducerLike, D: crate::ortho_database::OrthoDatabaseLike, H: crate::interner::InternerHolderLike>(
        &mut self,
        db: &mut D,
        dbq: &mut Q,     // destination for generated child orthos
        holder: &mut H,
    ) -> Result<(usize, usize, Option<std::time::Duration>), FoldError> { // (bumped, produced_children, diff_duration)
        use std::time::{SystemTime, UNIX_EPOCH};
        let ts_ms = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis();
        let versions = holder.versions();
        if versions.len() < 2 { 
            println!("[follower][iter] ts={} status=WAITING reason=INSUFFICIENT_VERSIONS versions={}", ts_ms, versions.len());
            std::thread::sleep(std::time::Duration::from_millis(100)); 
            return Ok((0,0,None)); 
        }
        let low_version = versions[0];
        let high_version = *versions.last().unwrap();
        if self.low_version != Some(low_version) { self.low_interner = holder.get(low_version); self.low_version = Some(low_version); }
        if self.high_version != Some(high_version) { self.high_interner = holder.get(high_version); self.high_version = Some(high_version); }
        if self.low_interner.is_none() || self.high_interner.is_none() { 
            println!("[follower][iter] ts={} status=WAITING reason=INTERNER_MISSING low_ok={} high_ok={}", ts_ms, self.low_interner.is_some(), self.high_interner.is_some());
            return Ok((0,0,None)); 
        }
        let candidate = db.sample_version(low_version).expect("queue connection failed");
        let ortho = match candidate { 
            Some(o) => o, 
            None => { 
                println!("[follower][iter] ts={} status=EMPTY reason=NO_MORE_ORTHOS low_version={}", ts_ms, low_version); 
                holder.delete(low_version); self.low_interner = None; self.low_version = None; return Ok((0,0,None)); 
            } 
        };
        let (forbidden, required) = ortho.get_requirements();
        let low = self.low_interner.as_ref().unwrap();
        let high = self.high_interner.as_ref().unwrap();
        let low_vocab_len = low.vocabulary().len();
        let high_vocab_len = high.vocabulary().len();
        let t_start = std::time::Instant::now();
        // Sparse additions collection: gather all new candidate indices (no threshold)
        use std::collections::HashSet;
        let mut additions: HashSet<usize> = HashSet::new();
        let forbidden_set: HashSet<usize> = forbidden.iter().copied().collect();
        if required.is_empty() {
            for i in low_vocab_len..high_vocab_len { if !forbidden_set.contains(&i) { additions.insert(i); } }
        } else {
            for prefix in &required {
                let high_bs_opt = high.completions_for_prefix(prefix);
                if high_bs_opt.is_none() { continue; }
                let high_bs = high_bs_opt.unwrap();
                let diffs = low.differing_completions_indices_up_to_vocab(high, prefix);
                for idx in diffs { if idx < low_vocab_len && !forbidden_set.contains(&idx) { if high_bs.contains(idx) { additions.insert(idx); } } }
                if high_vocab_len > low_vocab_len {
                    for idx in low_vocab_len..high_vocab_len { if forbidden_set.contains(&idx) { continue; } if high_bs.contains(idx) { additions.insert(idx); } }
                }
            }
        }
        if additions.is_empty() {
            println!("[follower][iter] ts={} status=BUMP reason=NO_ADDITIONS ortho_id={} low_version={} -> high_version={}", ts_ms, ortho.id(), low_version, high_version);
            let bumped = ortho.set_version(high_version); db.insert_or_update(bumped)?; return Ok((1,0,None));
        }
        let mut final_candidates: Vec<usize> = Vec::with_capacity(additions.len());
        'cand: for &cand in additions.iter() {
            if forbidden_set.contains(&cand) { continue; }
            for prefix in &required { if let Some(bs) = high.completions_for_prefix(prefix) { if !bs.contains(cand) { continue 'cand; } } else { continue 'cand; } }
            final_candidates.push(cand);
        }
        final_candidates.sort_unstable();
        let candidates_ct_for_log = final_candidates.len();
        let mut children = Vec::new();
        for completion in final_candidates { let mut batch = ortho.add(completion, high_version); children.append(&mut batch); }
        let produced = children.len();
        let elapsed = t_start.elapsed();
        if produced > 0 { dbq.push_many(children)?; }
        // Always log perf
        let additions_ct = additions.len();
        let candidates_ct = candidates_ct_for_log;
        let time_us = elapsed.as_micros();
        let per_add_us = if additions_ct>0 { time_us as f64 / additions_ct as f64 } else { 0.0 };
        let per_child_us = if produced>0 { time_us as f64 / produced as f64 } else { 0.0 };
        println!("[follower][iter] ts={} status=EXPAND additions={} candidates={} children={} time_us={} per_add_us={:.2} per_child_us={:.2} ortho_id={}", ts_ms, additions_ct, candidates_ct, produced, time_us, per_add_us, per_child_us, ortho.id());
        let bumped = ortho.set_version(high_version); db.insert_or_update(bumped)?;
        Ok((1, produced, Some(elapsed)))
    }
}

pub struct OrthoFeeder;

impl OrthoFeeder {
    #[instrument(skip_all)]
    pub fn run_feeder_once<D: crate::ortho_database::OrthoDatabaseLike, P:crate::queue::QueueProducerLike>(
        batch: &[crate::ortho::Ortho],
        db: &mut D,
        workq: &mut P,
    ) -> Result<(usize, usize), FoldError> { // (new, total)
        if batch.is_empty() { return Ok((0,0)); }
        static FEEDER_LRU: Lazy<Mutex<LruCache<usize, ()>>> = Lazy::new(|| {
            // Capacity chosen to balance memory (~16MB for 1M usize entries) vs hit rate; adjust via FOLD_FEEDER_LRU_CAP
            let cap = std::env::var("FOLD_FEEDER_LRU_CAP").ok()
                .and_then(|v| v.parse::<usize>().ok())
                .and_then(|c| std::num::NonZeroUsize::new(c))
                .unwrap_or_else(|| std::num::NonZeroUsize::new(1_000_000).unwrap());
            Mutex::new(LruCache::new(cap))
        });
        let total = batch.len();
        let mut cache_hits = 0usize;
        let mut candidates = Vec::with_capacity(total);
        {
            let mut lru = FEEDER_LRU.lock().unwrap();
            for o in batch.iter() {
                let id = o.id();
                if lru.contains(&id) {
                    cache_hits += 1;
                    continue;
                }
                // Insert before DB attempt so concurrent duplicates in same batch won't duplicate
                lru.put(id, ());
                candidates.push(o.clone());
            }
        }
        let skipped = cache_hits;
        if skipped > 0 {
            let pct_skipped = skipped as f64 / total as f64;
            println!("[feeder][cache] batch_total={} cache_hits={} attempted_db={} pct_skipped={:.4}", total, skipped, candidates.len(), pct_skipped);
        } else {
            println!("[feeder][cache] batch_total={} cache_hits=0 attempted_db={} pct_skipped=0.0000", total, candidates.len());
        }
        if candidates.is_empty() {
            return Ok((0, total));
        }
        let new_orthos = db.upsert(candidates)?; // only freshly inserted
        let new_count = new_orthos.len();
        workq.push_many(new_orthos)?;
        Ok((new_count, total))
    }
}

#[instrument(skip_all)]
pub fn process_worker_item_with_cached<P: crate::queue::QueueProducerLike>(
    ortho: &crate::ortho::Ortho,
    dbq: &mut P,
    interner: &crate::interner::Interner,
) -> Result<(), FoldError> {
    println!("[worker] Popped ortho from workq: id={}, version={}", ortho.id(), ortho.version());
    let (forbidden, required) = ortho.get_requirements();
    let completions = interner.intersect(&required, &forbidden);
    let version = interner.version();
    let mut new_orthos = Vec::new();
    for completion in completions {
        let mut batch = ortho.add(completion, version);
        new_orthos.append(&mut batch);
    }
    dbq.push_many(new_orthos)?;
    Ok(())
}

#[cfg(test)]
mod follower_diff_tests {
    use super::*;
    use crate::interner::InMemoryInternerHolder;
    use crate::queue::MockQueue;

    fn build_low_high(low_text: &str, high_text: &str) -> (crate::interner::Interner, crate::interner::Interner) {
        // low_text ingested first, then high_text appended to create new version
        let mut holder = InMemoryInternerHolder::new().unwrap();
        let mut q = MockQueue::new();
        holder.add_text_with_seed(low_text, &mut q).unwrap(); // version 1
        holder.add_text_with_seed(high_text, &mut q).unwrap(); // version 2
        let low = holder.get(1).unwrap();
        let high = holder.get(2).unwrap();
        (low, high)
    }

    #[test]
    fn test_delta_intersection_adds_only_new() {
        let (low, high) = build_low_high("a b", "a c"); // low has a b; high adds a c
        // Construct ortho with single token 'a' (index 0)
        let mut o = crate::ortho::Ortho::new(low.version());
        o = o.add(0, low.version()).pop().unwrap();
        let (forbidden, required) = o.get_requirements();
        assert!(forbidden.is_empty());
        assert_eq!(required, vec![vec![0]]);
        let low_set: std::collections::HashSet<usize> = low.intersect(&required, &forbidden).into_iter().collect();
        let high_set: std::collections::HashSet<usize> = high.intersect(&required, &forbidden).into_iter().collect();
        assert!(low_set.contains(&1)); // 'b'
        assert!(!low_set.contains(&2)); // 'c' absent in low
        assert!(high_set.contains(&1) && high_set.contains(&2));
        let delta: Vec<usize> = high_set.difference(&low_set).copied().collect();
        assert_eq!(delta, vec![2]);
    }

    #[test]
    fn test_delta_union_intersection_logic() {
        let (low, high) = build_low_high("a b", "a c");
        let mut o = crate::ortho::Ortho::new(low.version());
        o = o.add(0, low.version()).pop().unwrap();
        let (forbidden, required) = o.get_requirements();
        let low_set: std::collections::HashSet<usize> = low.intersect(&required, &forbidden).into_iter().collect();
        let high_set: std::collections::HashSet<usize> = high.intersect(&required, &forbidden).into_iter().collect();
        assert!(low_set.contains(&1));
        assert!(!low_set.contains(&2));
        assert!(high_set.contains(&2));
        let diff: Vec<usize> = high_set.difference(&low_set).copied().collect();
        assert_eq!(diff, vec![2]);
    }
}