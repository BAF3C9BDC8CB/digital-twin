//! dt-daemon proto 编译的构建脚本。
//!
//! 使用 tonic-build 将 .proto 定义编译为 Rust 代码。

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let proto_dir = std::path::Path::new("proto");
    let protos: Vec<_> = [
        "common.proto",
        "dt_core.proto",
        "embed.proto",
        "plugin_k8s.proto",
        "plugin_svc.proto",
        "plugin_jenkins.proto",
        "metrics.proto",
        "log.proto",
    ]
    .iter()
    .map(|f| proto_dir.join(f))
    .collect();

    tonic_build::configure()
        .build_server(true)
        .build_client(true)
        .compile_protos(
            &protos
                .iter()
                .map(|p| p.to_str().unwrap())
                .collect::<Vec<_>>(),
            &[proto_dir.to_str().unwrap()],
        )?;

    Ok(())
}
