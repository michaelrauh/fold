use fold::{OrthoFeeder, queue::Queue, ortho_database::PostgresOrthoDatabase};
use dotenv::dotenv;
use std::{thread, time::Duration};
use opentelemetry::trace::TracerProvider;
use tracing_subscriber::prelude::__tracing_subscriber_SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

fn main() {
    dotenv().ok();
    // Jaeger/OTLP tracing setup
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
        .with_attributes(vec![KeyValue::new("service.name", "fold-feeder")])
        .build();
    let tracer_provider = sdktrace::SdkTracerProvider::builder()
        .with_simple_exporter(otlp_exporter)
        .with_resource(resource)
        .build();
    let tracer = tracer_provider.tracer("fold-feeder");
    let otel_layer = tracing_opentelemetry::layer().with_tracer(tracer);
    tracing_subscriber::registry()
        .with(otel_layer)
        .with(tracing_subscriber::fmt::layer())
        .init();
    let mut dbq = Queue::new("dbq");
    let mut workq = Queue::new("workq");
    let mut db = PostgresOrthoDatabase::new();
    loop {
        OrthoFeeder::run_feeder_once(&mut dbq, &mut db, &mut workq);
        thread::sleep(Duration::from_millis(100));
    }
}
