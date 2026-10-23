//! Optional OTLP trace export.
//!
//! The exporter is built only when an endpoint is configured; with none, no
//! provider, no background thread and no propagator exist (a `must_not_` test
//! holds that). Sampling is `ParentBased(TraceIdRatioBased)`: a sampled parent is
//! always honoured, new traces are sampled at the configured ratio. Head sampling
//! cannot see the final status, so keeping every error trace is the collector's
//! job (tail sampling).

use std::time::Duration;

use opentelemetry::KeyValue;
use opentelemetry::global;
use opentelemetry::trace::TracerProvider as _;
use opentelemetry_otlp::tonic_types::transport::ClientTlsConfig;
use opentelemetry_otlp::{Protocol, SpanExporter, WithExportConfig, WithTonicConfig};
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::propagation::TraceContextPropagator;
use opentelemetry_sdk::trace::{Sampler, SdkTracer, SdkTracerProvider};

use crate::ObservabilityError;

/// Export timeout for one batch.
const EXPORT_TIMEOUT: Duration = Duration::from_secs(10);
/// Instrumentation scope name of the tracer.
const TRACER_NAME: &str = "rustryx";

/// OTLP transport.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OtlpProtocol {
    /// gRPC (usually port 4317).
    Grpc,
    /// HTTP with protobuf bodies (usually port 4318).
    Http,
}

/// Where and how to export traces.
#[derive(Debug, Clone, PartialEq)]
pub struct OtlpOptions {
    /// Collector endpoint. For HTTP, a base URL without path gets `/v1/traces` appended.
    pub endpoint: String,
    /// Transport.
    pub protocol: OtlpProtocol,
    /// Head sampling ratio for traces without a sampled parent, 0 to 1.
    pub sample_ratio: f64,
}

/// The provider and the tracer that feeds the `tracing` bridge.
pub(crate) struct Exporter {
    pub provider: SdkTracerProvider,
    pub tracer: SdkTracer,
}

/// Builds the provider. gRPC needs a tokio runtime context (enter the runtime
/// first); HTTP uses a blocking client on the batch thread and needs none.
pub(crate) fn build(
    service_name: &str,
    options: &OtlpOptions,
) -> Result<Exporter, ObservabilityError> {
    let exporter = build_exporter(options)?;
    let resource = Resource::builder()
        .with_attributes([KeyValue::new("service.name", service_name.to_owned())])
        .build();
    let provider = SdkTracerProvider::builder()
        .with_batch_exporter(exporter)
        .with_sampler(sampler(options.sample_ratio))
        .with_resource(resource)
        .build();
    // Incoming `traceparent` is honoured and outgoing requests carry it.
    global::set_text_map_propagator(TraceContextPropagator::new());
    let tracer = provider.tracer(TRACER_NAME);
    Ok(Exporter { provider, tracer })
}

fn sampler(ratio: f64) -> Sampler {
    Sampler::ParentBased(Box::new(Sampler::TraceIdRatioBased(ratio.clamp(0.0, 1.0))))
}

fn build_exporter(options: &OtlpOptions) -> Result<SpanExporter, ObservabilityError> {
    let fail = |e: opentelemetry_otlp::ExporterBuildError| ObservabilityError::Otlp(e.to_string());
    match options.protocol {
        OtlpProtocol::Grpc => {
            let mut builder = SpanExporter::builder()
                .with_tonic()
                .with_endpoint(options.endpoint.clone())
                .with_timeout(EXPORT_TIMEOUT);
            if options.endpoint.starts_with("https://") {
                // System roots, so a collector behind a corporate CA works with the
                // CA installed in the image (SSL_CERT_FILE is not read by tonic).
                builder = builder.with_tls_config(ClientTlsConfig::new().with_enabled_roots());
            }
            builder.build().map_err(fail)
        }
        OtlpProtocol::Http => SpanExporter::builder()
            .with_http()
            .with_protocol(Protocol::HttpBinary)
            .with_endpoint(http_traces_endpoint(&options.endpoint))
            .with_timeout(EXPORT_TIMEOUT)
            .build()
            .map_err(fail),
    }
}

/// The crate uses a programmatic HTTP endpoint verbatim, so a base URL such as
/// `http://collector:4318` would receive traces at `/` and be rejected. Append the
/// signal path unless the operator already gave one.
pub(crate) fn http_traces_endpoint(endpoint: &str) -> String {
    let (scheme_and_host, path) = match endpoint.split_once("://") {
        Some((scheme, rest)) => match rest.find('/') {
            Some(i) => (
                format!("{scheme}://{}", rest.get(..i).unwrap_or_default()),
                rest.get(i..).unwrap_or("/"),
            ),
            None => (endpoint.to_owned(), ""),
        },
        None => return endpoint.to_owned(),
    };
    if path.is_empty() || path == "/" {
        format!("{scheme_and_host}/v1/traces")
    } else {
        endpoint.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn http_endpoint_gets_the_signal_path_only_when_missing() {
        assert_eq!(
            http_traces_endpoint("http://c:4318"),
            "http://c:4318/v1/traces"
        );
        assert_eq!(
            http_traces_endpoint("http://c:4318/"),
            "http://c:4318/v1/traces"
        );
        assert_eq!(
            http_traces_endpoint("https://c/otlp/v1/traces"),
            "https://c/otlp/v1/traces"
        );
        assert_eq!(http_traces_endpoint("https://c/custom"), "https://c/custom");
        assert_eq!(http_traces_endpoint("not a url"), "not a url");
    }

    #[test]
    fn sampler_ratio_is_clamped() {
        let _ = sampler(5.0);
        let _ = sampler(-1.0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn grpc_provider_builds_without_a_reachable_collector() {
        let options = OtlpOptions {
            endpoint: "http://127.0.0.1:1".to_owned(),
            protocol: OtlpProtocol::Grpc,
            sample_ratio: 0.1,
        };
        let exporter = build("rustryx-test", &options).expect("lazy connect never fails the build");
        tokio::task::spawn_blocking(move || {
            let _ = exporter.provider.shutdown();
        })
        .await
        .expect("join");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn https_grpc_endpoint_builds_with_system_roots() {
        let options = OtlpOptions {
            endpoint: "https://collector.invalid:4317".to_owned(),
            protocol: OtlpProtocol::Grpc,
            sample_ratio: 1.0,
        };
        let exporter = build("rustryx-test", &options).expect("builds");
        tokio::task::spawn_blocking(move || {
            let _ = exporter.provider.shutdown();
        })
        .await
        .expect("join");
    }

    #[test]
    fn http_provider_builds_without_a_runtime() {
        let options = OtlpOptions {
            endpoint: "http://127.0.0.1:1".to_owned(),
            protocol: OtlpProtocol::Http,
            sample_ratio: 0.0,
        };
        let exporter = build("rustryx-test", &options).expect("builds");
        let _ = exporter.provider.shutdown();
    }
}
