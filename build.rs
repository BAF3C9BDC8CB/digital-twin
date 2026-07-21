//! Build script for dt-daemon proto compilation.
//!
//! Uses tonic-build to compile .proto definitions into Rust code.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let proto_dir = std::path::Path::new("proto");
    let protos: Vec<_> = [
        "common.proto",
        "dt_core.proto",
        "embed.proto",
        "inference.proto",
        "plugin_k8s.proto",
        "plugin_svc.proto",
        "plugin_jenkins.proto",
        "metrics.proto",
        "log.proto",
        "reranker.proto",
    ]
    .iter()
    .map(|f| proto_dir.join(f))
    .collect();

    tonic_build::configure()
        .build_server(true)
        .build_client(true)
        .compile_protos(
            &protos.iter().map(|p| p.to_str().unwrap()).collect::<Vec<_>>(),
            &[proto_dir.to_str().unwrap()],
        )?;

    Ok(())
}
