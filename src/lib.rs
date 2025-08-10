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
    let otlp_exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_http()
        .with_protocol(Protocol::HttpBinary)
        .with_endpoint("http://jaeger:4318/v1/traces")
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
    pub fn run_follower_once<P: crate::queue::QueueProducerLike, D: crate::ortho_database::OrthoDatabaseLike, H: crate::interner::InternerHolderLike>(
        &mut self,
        db: &mut D,
        workq: &mut P,
        holder: &mut H,
    ) -> Result<(usize, usize), FoldError> { // (bumped, requeued)
        let versions = holder.versions();
        if versions.len() < 2 {
            std::thread::sleep(std::time::Duration::from_millis(100));
            return Ok((0, 0));
        }

        let low_version = versions[0];
        let high_version = *versions.last().unwrap();

        if self.low_version != Some(low_version) {
            self.low_interner = holder.get(low_version);
            self.low_version = Some(low_version);
        }

        if self.high_version != Some(high_version) {
            self.high_interner = holder.get(high_version);
            self.high_version = Some(high_version);
        }

        let candidate = db.sample_version(low_version).expect("queue connection failed");
        let ortho = match candidate {
            Some(o) => o,
            None => {
                holder.delete(low_version);
                self.low_interner = None;
                self.low_version = None;
                return Ok((0, 0));
            }
        };
        
        let (_forbidden, prefixes) = ortho.get_requirements();
        let all_same = match (self.low_interner.as_ref(), self.high_interner.as_ref()) {
            (Some(low), Some(high)) => low.all_completions_equal_up_to_vocab(high, &prefixes),
            _ => false,
        };
        if all_same {
            let new_ortho = ortho.set_version(high_version);
            db.insert_or_update(new_ortho).expect("queue connection failed");
            Ok((1, 0))
        } else {
            let new_ortho = ortho.set_version(high_version);
            println!("[follower] Pushing ortho to workq: id={}, version={}", new_ortho.id(), new_ortho.version());
            workq.push_many(vec![new_ortho.clone()]).expect("queue connection failed");
            db.remove_by_id(&ortho.id())?;
            Ok((0, 1))
        }
    }

    #[instrument(skip_all)]
    pub fn process_ortho<P: crate::queue::QueueProducerLike, D: crate::ortho_database::OrthoDatabaseLike, H: crate::interner::InternerHolderLike>(
        &mut self,
        ortho: &crate::ortho::Ortho,
        db: &mut D,
        workq: &mut P,
        holder: &mut H,
    ) -> Result<(), FoldError> {
        let versions = holder.versions();
        if versions.len() < 2 {
            std::thread::sleep(std::time::Duration::from_millis(100));
            return Ok(());
        }
        let low_version = versions[0];
        let high_version = *versions.last().unwrap();
        if self.low_version != Some(low_version) {
            self.low_interner = holder.get(low_version);
            self.low_version = Some(low_version);
        }
        if self.high_version != Some(high_version) {
            self.high_interner = holder.get(high_version);
            self.high_version = Some(high_version);
        }
        let (_forbidden, prefixes) = ortho.get_requirements();
        let all_same = match (self.low_interner.as_ref(), self.high_interner.as_ref()) {
            (Some(low), Some(high)) => low.all_completions_equal_up_to_vocab(high, &prefixes),
            _ => false,
        };
        if all_same {
            let new_ortho = ortho.set_version(high_version);
            db.insert_or_update(new_ortho).expect("queue connection failed");
        } else {
            let new_ortho = ortho.set_version(high_version);
            workq.push_many(vec![new_ortho.clone()]).expect("queue connection failed");
            db.remove_by_id(&ortho.id())?;
        }
        Ok(())
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