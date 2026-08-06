//! Nacos 虚拟文件源 —— 拉取 Nacos 配置并生成 VirtualFile 列表。
//!
//! 复用 NacosClient 的 HTTP API，遍历每个命名空间的配置条目，
//! 将每条配置构造为 `VirtualFile`（`source = Nacos`，`mtime = None`，
//! `content_hash = SHA256(content)`）。跳过 `old-*`/`public`/空命名空间
//!（沿用 `ConfigSyncSource` 的过滤逻辑）。
//!
//! 使用方式：
//!
//! ```ignore
//! let client = NacosClient::new("https://nacos.example.com/nacos");
//! let source = NacosVirtualFileSource::new(client);
//! let vfiles = source.fetch_virtual_files("my-project").await?;
//! // vfiles: Vec<VirtualFile>, 每个元素的 virtual_path = dt://nacos/{ns}/{data_id}.yaml
//! ```

use crate::application::pipeline::virtual_file::{FileSourceKind, VirtualFile};
use crate::application::sync::nacos::client::NacosClient;
use crate::domain::error::DtError;
use sha2::{Digest, Sha256};

/// Nacos 配置的虚拟文件源。
///
/// 将 Nacos 配置中心的所有配置转换为 `VirtualFile` 列表，供流水线增量处理使用。
#[derive(Debug, Clone)]
pub struct NacosVirtualFileSource {
    client: NacosClient,
}

impl NacosVirtualFileSource {
    /// 使用指定的 NacosClient 创建新的虚拟文件源。
    pub fn new(client: NacosClient) -> Self {
        Self { client }
    }

    /// 拉取所有命名空间的配置并返回 VirtualFile 列表。
    ///
    /// - 跳过 `old-*`、`public` 命名空间（与 `ConfigSyncSource` 一致）
    /// - 跳过 `config_count == 0` 的空命名空间
    /// - 每个配置条目构造为 `VirtualFile`，`virtual_path = dt://nacos/{namespace_id}/{data_id}.yaml`
    /// - `content_hash` 为内容的 SHA256 十六进制编码
    /// - `mtime` 为 `None`（Nacos 无文件修改时间概念）
    pub async fn fetch_virtual_files(
        &self,
        project: &str,
    ) -> Result<Vec<VirtualFile>, DtError> {
        // 1. 获取所有命名空间
        let ns_resp = self.client.list_namespaces().await?;
        let mut all_files: Vec<VirtualFile> = Vec::new();

        for ns in &ns_resp.data {
            let ns_id = &ns.namespace_id;
            let ns_name = &ns.namespace_show_name;

            // 跳过 old-* / public / 空命名空间（沿用 config_sync 逻辑）
            if ns_name.starts_with("old-") || ns_name == "public" || ns.config_count == 0 {
                continue;
            }

            // 2. 分页拉取配置清单
            let mut page: i64 = 1;
            let page_size: i64 = 100;

            loop {
                let list = match self.client.list_configs(ns_id, page, page_size).await? {
                    Some(l) => l,
                    None => break,
                };

                for cfg_item in &list.page_items {
                    // 3. 获取配置详情（内容）
                    let detail = self
                        .client
                        .get_config_detail(&cfg_item.data_id, &cfg_item.group, ns_id)
                        .await?;
                    let content = detail.content.unwrap_or_default();

                    // 4. 计算 content_hash（SHA256）
                    let content_hash = {
                        let mut h = Sha256::new();
                        h.update(content.as_bytes());
                        hex::encode(h.finalize())
                    };

                    // 5. 构造 VirtualFile
                    let virtual_path =
                        format!("dt://nacos/{}/{}.yaml", ns_id, cfg_item.data_id);

                    let vf = VirtualFile::new(
                        virtual_path,
                        content,
                        project.to_string(),
                        FileSourceKind::Nacos,
                        None, // Nacos 无 mtime
                        content_hash,
                    );

                    all_files.push(vf);
                }

                page += 1;
            }
        }

        Ok(all_files)
    }
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fetch_virtual_files_no_client_panics_on_build() {
        // 仅验证类型可以构造（需要实际 Nacos 服务器做集成测试）
        let client = NacosClient::new("http://localhost:8848/nacos");
        let source = NacosVirtualFileSource::new(client);
        // 确保 source 持有 client
        let _ = source;
    }

    /// 验证构造的 VirtualFile 各字段正确
    #[test]
    fn virtual_file_from_nacos_fields() {
        let vf = VirtualFile::new(
            "dt://nacos/prod/app.yaml",
            "server.port: 8080",
            "my-project",
            FileSourceKind::Nacos,
            None,
            "abc123def",
        );
        assert_eq!(vf.virtual_path, "dt://nacos/prod/app.yaml");
        assert_eq!(vf.content, "server.port: 8080");
        assert_eq!(vf.project, "my-project");
        assert_eq!(vf.source, FileSourceKind::Nacos);
        assert!(!vf.source.is_fs());
        assert!(vf.mtime.is_none());
        assert_eq!(vf.content_hash, "abc123def");
        assert!(vf.front_matter.is_none());
    }

    /// content_hash 应为有效 SHA256 hex（64 字符）
    #[test]
    fn content_hash_is_sha256_hex() {
        use sha2::Digest;
        let mut h = Sha256::new();
        h.update(b"test content");
        let hash = hex::encode(h.finalize());
        assert_eq!(hash.len(), 64);
        // 确认全部为 hex 字符
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
