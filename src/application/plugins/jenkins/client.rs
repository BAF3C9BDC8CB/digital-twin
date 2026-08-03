//! Jenkins HTTP API 客户端——直接 REST 调用（不依赖外部二进制）。
//!
//! 通过标准 REST API 与 Jenkins 通信：
//! - `GET /api/json` → 作业列表
//! - `GET /job/{name}/api/json` → 作业详情
//! - `GET /job/{name}/{build}/api/json` → 构建信息
//! - `POST /job/{name}/buildWithParameters` → 触发构建
//! - `GET /job/{name}/{build}/consoleText` → 构建日志

use crate::domain::error::DtError;

/// 基于 reqwest 的异步 Jenkins API 客户端。
pub struct JenkinsApiClient {
    base_url: String,
    user: String,
    token: String,
    http: reqwest::Client,
}

impl JenkinsApiClient {
    /// 创建新客户端。
    ///
    /// `base_url` 应为 Jenkins 根 URL，例如 `http://jenkins.example.com:8080`。
    pub fn new(base_url: &str, user: &str, token: &str) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            user: user.to_string(),
            token: token.to_string(),
            http: reqwest::Client::new(),
        }
    }

    /// GET 请求一个 JSON 端点。
    async fn get_json(&self, path: &str) -> Result<serde_json::Value, DtError> {
        let url = format!("{}{}", self.base_url, path);
        let resp = self
            .http
            .get(&url)
            .basic_auth(&self.user, Some(&self.token))
            .send()
            .await
            .map_err(|e| DtError::Network(format!("Jenkins 请求失败 {url}: {e}")))?;

        if !resp.status().is_success() {
            return Err(DtError::Network(format!(
                "Jenkins HTTP {} (url={url})",
                resp.status()
            )));
        }

        resp.json()
            .await
            .map_err(|e| DtError::Network(format!("Jenkins JSON 解析失败 {url}: {e}")))
    }

    /// GET 请求原始文本端点。
    async fn get_text(&self, path: &str) -> Result<String, DtError> {
        let url = format!("{}{}", self.base_url, path);
        let resp = self
            .http
            .get(&url)
            .basic_auth(&self.user, Some(&self.token))
            .send()
            .await
            .map_err(|e| DtError::Network(format!("Jenkins 请求失败 {url}: {e}")))?;

        if !resp.status().is_success() {
            return Err(DtError::Network(format!(
                "Jenkins HTTP {} (url={url})",
                resp.status()
            )));
        }

        resp.text()
            .await
            .map_err(|e| DtError::Network(format!("Jenkins 读取失败 {url}: {e}")))
    }

    /// POST 请求，支持可选表单参数。
    async fn post_with_params(
        &self,
        path: &str,
        params: &[(&str, &str)],
    ) -> Result<String, DtError> {
        let url = format!("{}{}", self.base_url, path);
        let mut form = Vec::new();
        for (k, v) in params {
            form.push(format!("{}={}", urlencoding(k), urlencoding(v)));
        }
        let body = form.join("&");

        let resp = self
            .http
            .post(&url)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .basic_auth(&self.user, Some(&self.token))
            .body(body)
            .send()
            .await
            .map_err(|e| DtError::Network(format!("Jenkins POST 失败 {url}: {e}")))?;

        if !resp.status().is_success() && resp.status().as_u16() != 201 {
            return Err(DtError::Network(format!(
                "Jenkins HTTP {} (url={url})",
                resp.status()
            )));
        }

        Ok(format!(
            "已触发构建: {}/queue/item/{}/",
            self.base_url,
            resp.status()
        ))
    }

    /// 列出所有 Jenkins 作业。
    pub async fn list_jobs(&self) -> Result<String, DtError> {
        let json = self.get_json("/api/json?tree=jobs[name,color]").await?;

        let mut out = String::new();
        out.push_str(&format!("{:<50} {:<10}\n", "作业", "状态"));
        if let Some(jobs) = json["jobs"].as_array() {
            if jobs.is_empty() {
                return Ok("(无作业)".into());
            }
            for job in jobs {
                let name = job["name"].as_str().unwrap_or("?");
                let color = job["color"].as_str().unwrap_or("?");
                let status = match color {
                    "blue" | "blue_anime" => "正常",
                    "red" | "red_anime" => "失败",
                    "yellow" | "yellow_anime" => "不稳定",
                    "aborted" | "aborted_anime" => "已中止",
                    "notbuilt" | "notbuilt_anime" => "未构建",
                    "disabled" | "disabled_anime" => "已禁用",
                    _ => color,
                };
                out.push_str(&format!("{:<50} {:<10}\n", name, status));
            }
        }
        Ok(out)
    }

    /// 显示某个作业的构建参数。
    pub async fn get_params(&self, job: &str) -> Result<String, DtError> {
        let encoded = urlencoding(job);
        let json = self
            .get_json(&format!(
                "/job/{}/api/json?tree=property[parameterDefinitions[name,type,defaultParameterValue[value],description,choices]]",
                encoded
            ))
            .await?;

        let mut out = String::new();
        out.push_str(&format!("作业 {job} 的参数:\n"));
        out.push_str(&format!(
            "{:<25} {:<15} {:<15} {:<30}\n",
            "名称", "类型", "默认值", "描述"
        ));

        if let Some(props) = json["property"].as_array() {
            for prop in props {
                if let Some(defs) = prop["parameterDefinitions"].as_array() {
                    for def in defs {
                        let name = def["name"].as_str().unwrap_or("?");
                        let ptype = def["type"].as_str().unwrap_or("?");
                        let default = def["defaultParameterValue"]["value"]
                            .as_str()
                            .unwrap_or("-");
                        let desc = def["description"].as_str().unwrap_or("-");
                        out.push_str(&format!(
                            "{:<25} {:<15} {:<15} {:<30}\n",
                            name, ptype, default, desc,
                        ));
                    }
                }
            }
        }

        if out.lines().count() <= 2 {
            out.push_str("(无参数)\n");
        }
        Ok(out)
    }

    /// 显示某个作业的构建历史。
    pub async fn get_history(&self, job: &str, limit: Option<u32>) -> Result<String, DtError> {
        let limit = limit.unwrap_or(10);
        let encoded = urlencoding(job);
        let json = self
            .get_json(&format!(
                "/job/{}/api/json?tree=builds[number,result,timestamp,duration,url]{{0,{}}}",
                encoded, limit
            ))
            .await?;

        let mut out = String::new();
        out.push_str(&format!("作业 {job} 的构建历史:\n"));
        out.push_str(&format!(
            "{:<8} {:<10} {:<12} {:<20}\n",
            "构建", "结果", "耗时", "时间戳"
        ));

        if let Some(builds) = json["builds"].as_array() {
            if builds.is_empty() {
                return Ok(format!("{out}(无构建)\n"));
            }
            for build in builds {
                let num = build["number"]
                    .as_i64()
                    .map_or("?".to_string(), |n| n.to_string());
                let result = build["result"].as_str().unwrap_or("运行中");
                let duration_ms = build["duration"].as_i64().unwrap_or(0);
                let duration = format_duration(duration_ms);
                let ts = build["timestamp"]
                    .as_i64()
                    .map_or("-".to_string(), |t| format_epoch_ms(t));
                out.push_str(&format!(
                    "{:<8} {:<10} {:<12} {:<20}\n",
                    num, result, duration, ts,
                ));
            }
        } else {
            out.push_str("(无构建)\n");
        }
        Ok(out)
    }

    /// 获取指定构建的控制台输出。
    pub async fn get_build_log(&self, job: &str, build: Option<&str>) -> Result<String, DtError> {
        let encoded_job = urlencoding(job);
        let build_id = build.unwrap_or("lastBuild");
        let text = self
            .get_text(&format!("/job/{}/{}/consoleText", encoded_job, build_id))
            .await?;
        Ok(text)
    }

    /// 触发某个作业的构建。
    pub async fn trigger_build(
        &self,
        job: &str,
        params: &[(&str, &str)],
    ) -> Result<String, DtError> {
        let encoded = urlencoding(job);
        self.post_with_params(&format!("/job/{}/buildWithParameters", encoded), params)
            .await
    }
}

// ── 用于 jc-sync 的结构化响应类型 ───────────────────────────────

/// 用于同步的结构化 Jenkins 视图信息（包含嵌套作业）。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct JenkinsViewInfo {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub jobs: Vec<JenkinsJobInfo>,
}

/// 用于同步的结构化 Jenkins 作业信息。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct JenkinsJobInfo {
    pub name: String,
    pub url: String,
    #[serde(default)]
    pub color: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub full_name: String,
}

/// 用于同步的结构化 Jenkins 构建信息。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct JenkinsBuildInfo {
    pub number: i64,
    pub result: Option<String>,
    pub timestamp: i64,
    pub duration: i64,
    pub url: String,
}

impl JenkinsApiClient {
    /// 获取所有作业的扁平列表（供 jc-sync 兜底使用）。
    ///
    /// 某些 Jenkins 视图类型（Dashboard、Nested View）会在响应中省略
    /// `jobs` 数组。该端点无论如何都会返回全部作业，
    /// 确保覆盖完整。
    pub async fn list_all_jobs(&self) -> Result<Vec<JenkinsJobInfo>, DtError> {
        let json = self
            .get_json("/api/json?tree=jobs[name,url,color,description,fullName]")
            .await?;
        let jobs: Vec<JenkinsJobInfo> = json["jobs"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .map(|j| {
                        serde_json::from_value(j.clone()).unwrap_or_else(|_| JenkinsJobInfo {
                            name: j["name"].as_str().unwrap_or("?").to_string(),
                            url: j["url"].as_str().unwrap_or("").to_string(),
                            color: j["color"].as_str().unwrap_or("").to_string(),
                            description: j["description"].as_str().unwrap_or("").to_string(),
                            full_name: j["fullName"].as_str().unwrap_or("").to_string(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ok(jobs)
    }

    /// 获取所有视图及其嵌套作业（供 jc-sync 使用）。
    ///
    /// 视图提供 CONTAINS 映射：每个视图的 `jobs` 数组告诉我们
    /// 哪些作业属于该视图。完整的扁平列表请使用 [`list_all_jobs`]。
    ///
    /// 调用 `/api/json?tree=views[name,description,jobs[...]]`，
    /// 返回真实的 Jenkins 视图（如 `JAVA`、`JAVA-TEST`、`VUE`）及其作业。
    pub async fn list_views(&self) -> Result<Vec<JenkinsViewInfo>, DtError> {
        let json = self
            .get_json(
                "/api/json?tree=views[name,description,jobs[name,url,color,description,fullName]]",
            )
            .await?;
        let views: Vec<JenkinsViewInfo> = json["views"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .map(|v| {
                        serde_json::from_value(v.clone()).unwrap_or_else(|_| JenkinsViewInfo {
                            name: v["name"].as_str().unwrap_or("?").to_string(),
                            description: v["description"].as_str().unwrap_or("").to_string(),
                            url: String::new(),
                            jobs: v["jobs"]
                                .as_array()
                                .map(|jarr| {
                                    jarr.iter()
                                        .map(|j| {
                                            serde_json::from_value(j.clone()).unwrap_or_else(|_| {
                                                JenkinsJobInfo {
                                                    name: j["name"]
                                                        .as_str()
                                                        .unwrap_or("?")
                                                        .to_string(),
                                                    url: j["url"]
                                                        .as_str()
                                                        .unwrap_or("")
                                                        .to_string(),
                                                    color: j["color"]
                                                        .as_str()
                                                        .unwrap_or("")
                                                        .to_string(),
                                                    description: j["description"]
                                                        .as_str()
                                                        .unwrap_or("")
                                                        .to_string(),
                                                    full_name: j["fullName"]
                                                        .as_str()
                                                        .unwrap_or("")
                                                        .to_string(),
                                                }
                                            })
                                        })
                                        .collect()
                                })
                                .unwrap_or_default(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ok(views)
    }

    /// 获取某个作业的全部构建（供 jc-sync 使用）。
    ///
    /// 使用 `full_name` 构造文件夹限定的路径。若 `full_name` 为空
    /// 则回退到 `job_name`（某些 Jenkins 作业类型省略 fullName）。
    pub async fn get_all_builds(
        &self,
        job_name: &str,
        full_name: &str,
    ) -> Result<Vec<JenkinsBuildInfo>, DtError> {
        let name = if full_name.is_empty() {
            job_name
        } else {
            full_name
        };
        let path = name.replace('/', "/job/");
        let json = self
            .get_json(&format!(
                "/job/{}/api/json?tree=builds[number,result,timestamp,duration,url]",
                path
            ))
            .await?;
        let builds: Vec<JenkinsBuildInfo> = json["builds"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .map(|b| {
                        serde_json::from_value(b.clone()).unwrap_or_else(|_| JenkinsBuildInfo {
                            number: b["number"].as_i64().unwrap_or(0),
                            result: b["result"].as_str().map(|s| s.to_string()),
                            timestamp: b["timestamp"].as_i64().unwrap_or(0),
                            duration: b["duration"].as_i64().unwrap_or(0),
                            url: b["url"].as_str().unwrap_or("").to_string(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ok(builds)
    }
}

impl Default for JenkinsApiClient {
    fn default() -> Self {
        Self::new("http://localhost:8080", "", "")
    }
}

// ── Helpers ─────────────────────────────────────────────────────────────────

fn urlencoding(s: &str) -> String {
    // 简易 URL 编码——避免引入 `urlencoding` crate
    s.replace('%', "%25")
        .replace(' ', "%20")
        .replace('#', "%23")
        .replace('&', "%26")
        .replace('?', "%3F")
        .replace('=', "%3D")
}

fn format_duration(ms: i64) -> String {
    if ms < 1000 {
        format!("{}ms", ms)
    } else if ms < 60_000 {
        format!("{:.1}s", ms as f64 / 1000.0)
    } else if ms < 3_600_000 {
        format!("{:.1}m", ms as f64 / 60_000.0)
    } else {
        format!("{:.1}h", ms as f64 / 3_600_000.0)
    }
}

fn format_epoch_ms(ms: i64) -> String {
    if let Some(dt) = chrono::DateTime::from_timestamp_millis(ms) {
        dt.format("%Y-%m-%d %H:%M:%S").to_string()
    } else {
        ms.to_string()
    }
}
