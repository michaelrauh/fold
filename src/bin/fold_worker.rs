use fold::{run_worker_once, queue::Queue, interner::BlobInternerHolder};
use dotenv::dotenv;
use std::{thread, time::Duration};
use tracing_subscriber::prelude::__tracing_subscriber_SubscriberExt;
use opentelemetry::trace::TracerProvider;
use fold::interner::InternerHolderLike;
use tracing_subscriber::util::SubscriberInitExt;

fn main() {
    dotenv().ok();
    // Always use Jaeger/OTLP tracing
    use opentelemetry::{KeyValue};
    use opentelemetry_sdk::{Resource, trace as sdktrace};
    use opentelemetry_otlp::{Protocol, WithExportConfig};
    let otlp_exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_http()
        .with_protocol(Protocol::HttpBinary)
        .with_endpoint("http://jaeger:4318/v1/traces")
        .build()
        .expect("Failed to build OTLP exporter");
    let resource = Resource::builder_empty()
        .with_attributes(vec![KeyValue::new("service.name", "fold-worker")])
        .build();
    let tracer_provider = sdktrace::SdkTracerProvider::builder()
        .with_simple_exporter(otlp_exporter)
        .with_resource(resource)
        .build();
    let tracer = tracer_provider.tracer("fold-worker");
    let otel_layer = tracing_opentelemetry::layer().with_tracer(tracer);
    tracing_subscriber::registry()
        .with(otel_layer)
        .with(tracing_subscriber::fmt::layer())
        .init();
    let mut workq = Queue::new("workq").expect("Failed to create workq");
    let mut dbq = Queue::new("dbq").expect("Failed to create dbq");
    let mut holder = BlobInternerHolder::new_internal().expect("Failed to create BlobInternerHolder");
    // Wait until there is at least one interner in the holder
    while holder.versions().len() == 0 {
        println!("[worker] Waiting for interner to be seeded...");
        thread::sleep(Duration::from_secs(1));
    }
    loop {
        run_worker_once(&mut workq, &mut dbq, &mut holder);
        // Optionally add sleep or exit condition if needed
    }
}
