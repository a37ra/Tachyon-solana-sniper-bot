use anyhow::Result;
use opentelemetry::KeyValue;
use opentelemetry_sdk::trace::{self, RandomIdGenerator, Sampler};
use opentelemetry_sdk::Resource;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::Registry;

pub fn init_telemetry(service_name: &str, otlp_endpoint: &str) -> Result<()> {
    let tracer = opentelemetry_otlp::new_pipeline()
        .tracing()
        .with_exporter(
            opentelemetry_otlp::new_exporter()
                .tonic()
                .with_endpoint(otlp_endpoint),
        )
        .with_trace_config(
            trace::config()
                .with_sampler(Sampler::AlwaysOn)
                .with_id_generator(RandomIdGenerator::default())
                .with_resource(Resource::new(vec![KeyValue::new(
                    "service.name",
                    service_name.to_string(),
                )])),
        )
        .install_batch(opentelemetry_sdk::runtime::Tokio)?;

    let telemetry = tracing_opentelemetry::layer().with_tracer(tracer);

    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_target(false)
        .with_thread_ids(true)
        .with_ansi(false);

    let subscriber = Registry::default()
        .with(telemetry)
        .with(fmt_layer)
        .with(tracing_subscriber::EnvFilter::from_default_env());

    if tracing::subscriber::set_global_default(subscriber).is_err() {
        tracing::warn!(endpoint = %otlp_endpoint, "OpenTelemetry subscriber already initialized; skipping duplicate init");
    }

    Ok(())
}
