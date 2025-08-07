use fold::{Follower, PostgresOrthoDatabase};
use fold::queue::Queue;
use fold::interner::{BlobInternerHolder, InternerHolderLike};
use dotenv::dotenv;
use opentelemetry::{KeyValue};
use opentelemetry_sdk::{Resource, trace as sdktrace};
use opentelemetry_otlp::{Protocol, WithExportConfig};
use tracing_opentelemetry;
use tracing_subscriber::prelude::__tracing_subscriber_SubscriberExt;
use opentelemetry::trace::TracerProvider;
use tracing_subscriber::util::SubscriberInitExt;

fn main() {
    dotenv().ok();
    // Always use Jaeger/OTLP tracing
    let otlp_exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_http()
        .with_protocol(Protocol::HttpBinary)
        .with_endpoint("http://jaeger:4318/v1/traces")
        .build()
        .expect("Failed to build OTLP exporter");
    let resource = Resource::builder_empty()
        .with_attributes(vec![KeyValue::new("service.name", "fold-follower")])
        .build();
    let tracer_provider = sdktrace::SdkTracerProvider::builder()
        .with_simple_exporter(otlp_exporter)
        .with_resource(resource)
        .build();
    let tracer = tracer_provider.tracer("fold-follower");
    let otel_layer = tracing_opentelemetry::layer().with_tracer(tracer);
    tracing_subscriber::registry()
        .with(otel_layer)
        .with(tracing_subscriber::fmt::layer())
        .init();
    let mut workq = Queue::new("workq").expect("Failed to create workq");
    let mut holder = BlobInternerHolder::new().expect("Failed to create BlobInternerHolder");
    let mut db = PostgresOrthoDatabase::new();
    let mut follower = Follower::new();
    loop {
        if let Err(e) = follower.run_follower_once(&mut db, &mut workq, &mut holder) {
            panic!("Follower error: {}", e);
        }
    }
}

// todo look at scalability issue - the random sampled ortho will likely always be the same
