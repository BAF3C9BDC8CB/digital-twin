//! Jenkins HTTP API client — direct REST calls (no external binary).
//!
//! Communicates with Jenkins via standard REST API:
//! - `GET /api/json` → job list
//! - `GET /job/{name}/api/json` → job details
//! - `GET /job/{name}/{build}/api/json` → build info
//! - `POST /job/{name}/buildWithParameters` → trigger build
//! - `GET /job/{name}/{build}/consoleText` → build log

use crate::domain::error::DtError;

/// Async Jenkins API client using reqwest.
pub struct JenkinsApiClient {
    base_url: String,
    user: String,
    token: String,
    http: reqwest::Client,
}

impl JenkinsApiClient {
    /// Create a new client.
    ///
    /// `base_url` should be the Jenkins root URL, e.g. `http://jenkins.example.com:8080`.
    pub fn new(base_url: &str, user: &str, token: &str) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            user: user.to_string(),
            token: token.to_string(),
            http: reqwest::Client::new(),
        }
    }

    /// GET a JSON endpoint.
    async fn get_json(&self, path: &str) -> Result<serde_json::Value, DtError> {
        let url = format!("{}{}", self.base_url, path);
        let resp = self
            .http
            .get(&url)
            .basic_auth(&self.user, Some(&self.token))
            .send()
            .await
            .map_err(|e| DtError::Network(format!("Jenkins request {url}: {e}")))?;

        if !resp.status().is_success() {
            return Err(DtError::Network(format!(
                "Jenkins HTTP {} for {url}",
                resp.status()
            )));
        }

        resp.json()
            .await
            .map_err(|e| DtError::Network(format!("Jenkins JSON parse {url}: {e}")))
    }

    /// GET raw text endpoint.
    async fn get_text(&self, path: &str) -> Result<String, DtError> {
        let url = format!("{}{}", self.base_url, path);
        let resp = self
            .http
            .get(&url)
            .basic_auth(&self.user, Some(&self.token))
            .send()
            .await
            .map_err(|e| DtError::Network(format!("Jenkins request {url}: {e}")))?;

        if !resp.status().is_success() {
            return Err(DtError::Network(format!(
                "Jenkins HTTP {} for {url}",
                resp.status()
            )));
        }

        resp.text()
            .await
            .map_err(|e| DtError::Network(format!("Jenkins read {url}: {e}")))
    }

    /// POST with optional form parameters.
    async fn post_with_params(
        &self,
        path: &str,
        params: &[(&str, &str)],
    ) -> Result<String, DtError> {
        let url = format!("{}{}", self.base_url, path);
        let mut form = Vec::new();
        for (k, v) in params {
            form.push(format!(
                "{}={}",
                urlencoding(k),
                urlencoding(v)
            ));
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
            .map_err(|e| DtError::Network(format!("Jenkins POST {url}: {e}")))?;

        if !resp.status().is_success() && resp.status().as_u16() != 201 {
            return Err(DtError::Network(format!(
                "Jenkins HTTP {} for {url}",
                resp.status()
            )));
        }

        Ok(format!(
            "Build triggered: {}/queue/item/{}/",
            self.base_url,
            resp.status()
        ))
    }

    /// List all Jenkins jobs.
    pub async fn list_jobs(&self) -> Result<String, DtError> {
        let json = self.get_json("/api/json?tree=jobs[name,color]").await?;

        let mut out = String::new();
        out.push_str(&format!("{:<50} {:<10}\n", "JOB", "STATUS"));
        if let Some(jobs) = json["jobs"].as_array() {
            if jobs.is_empty() {
                return Ok("(no jobs)".into());
            }
            for job in jobs {
                let name = job["name"].as_str().unwrap_or("?");
                let color = job["color"].as_str().unwrap_or("?");
                let status = match color {
                    "blue" | "blue_anime" => "OK",
                    "red" | "red_anime" => "FAIL",
                    "yellow" | "yellow_anime" => "UNSTABLE",
                    "aborted" | "aborted_anime" => "ABORTED",
                    "notbuilt" | "notbuilt_anime" => "NOT BUILT",
                    "disabled" | "disabled_anime" => "DISABLED",
                    _ => color,
                };
                out.push_str(&format!("{:<50} {:<10}\n", name, status));
            }
        }
        Ok(out)
    }

    /// Show build parameters for a job.
    pub async fn get_params(&self, job: &str) -> Result<String, DtError> {
        let encoded = urlencoding(job);
        let json = self
            .get_json(&format!(
                "/job/{}/api/json?tree=property[parameterDefinitions[name,type,defaultParameterValue[value],description,choices]]",
                encoded
            ))
            .await?;

        let mut out = String::new();
        out.push_str(&format!("Parameters for job: {job}\n"));
        out.push_str(&format!(
            "{:<25} {:<15} {:<15} {:<30}\n",
            "NAME", "TYPE", "DEFAULT", "DESCRIPTION"
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
            out.push_str("(no parameters)\n");
        }
        Ok(out)
    }

    /// Show build history for a job.
    pub async fn get_history(
        &self,
        job: &str,
        limit: Option<u32>,
    ) -> Result<String, DtError> {
        let limit = limit.unwrap_or(10);
        let encoded = urlencoding(job);
        let json = self
            .get_json(&format!(
                "/job/{}/api/json?tree=builds[number,result,timestamp,duration,url]{{0,{}}}",
                encoded, limit
            ))
            .await?;

        let mut out = String::new();
        out.push_str(&format!("Build history for: {job}\n"));
        out.push_str(&format!(
            "{:<8} {:<10} {:<12} {:<20}\n",
            "BUILD", "RESULT", "DURATION", "TIMESTAMP"
        ));

        if let Some(builds) = json["builds"].as_array() {
            if builds.is_empty() {
                return Ok(format!("{out}(no builds)\n"));
            }
            for build in builds {
                let num = build["number"].as_i64().map_or("?".to_string(), |n| n.to_string());
                let result = build["result"].as_str().unwrap_or("RUNNING");
                let duration_ms = build["duration"].as_i64().unwrap_or(0);
                let duration = format_duration(duration_ms);
                let ts = build["timestamp"].as_i64().map_or("-".to_string(), |t| {
                    format_epoch_ms(t)
                });
                out.push_str(&format!(
                    "{:<8} {:<10} {:<12} {:<20}\n",
                    num, result, duration, ts,
                ));
            }
        } else {
            out.push_str("(no builds)\n");
        }
        Ok(out)
    }

    /// Get console output for a specific build.
    pub async fn get_build_log(
        &self,
        job: &str,
        build: Option<&str>,
    ) -> Result<String, DtError> {
        let encoded_job = urlencoding(job);
        let build_id = build.unwrap_or("lastBuild");
        let text = self
            .get_text(&format!(
                "/job/{}/{}/consoleText",
                encoded_job, build_id
            ))
            .await?;
        Ok(text)
    }

    /// Trigger a build for a job.
    pub async fn trigger_build(
        &self,
        job: &str,
        params: &[(&str, &str)],
    ) -> Result<String, DtError> {
        let encoded = urlencoding(job);
        self.post_with_params(
            &format!("/job/{}/buildWithParameters", encoded),
            params,
        )
        .await
    }
}

// ── Structured response types for jc-sync ───────────────────────────────

/// Structured Jenkins job info for sync.
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

/// Structured Jenkins build info for sync.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct JenkinsBuildInfo {
    pub number: i64,
    pub result: Option<String>,
    pub timestamp: i64,
    pub duration: i64,
    pub url: String,
}

impl JenkinsApiClient {
    /// Fetch all jobs with full details (for jc-sync).
    pub async fn list_all_jobs(&self) -> Result<Vec<JenkinsJobInfo>, DtError> {
        let json = self
            .get_json("/api/json?tree=jobs[name,url,color,description,fullName]")
            .await?;
        let jobs: Vec<JenkinsJobInfo> = json["jobs"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .map(|j| serde_json::from_value(j.clone()).unwrap_or_else(|_| JenkinsJobInfo {
                        name: j["name"].as_str().unwrap_or("?").to_string(),
                        url: j["url"].as_str().unwrap_or("").to_string(),
                        color: j["color"].as_str().unwrap_or("").to_string(),
                        description: j["description"].as_str().unwrap_or("").to_string(),
                        full_name: j["fullName"].as_str().unwrap_or("").to_string(),
                    }))
                    .collect()
            })
            .unwrap_or_default();
        Ok(jobs)
    }

    /// Fetch all builds for a job (for jc-sync).
    pub async fn get_all_builds(&self, _job_name: &str, full_name: &str) -> Result<Vec<JenkinsBuildInfo>, DtError> {
        let path = full_name.replace('/', "/job/");
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
                    .map(|b| serde_json::from_value(b.clone()).unwrap_or_else(|_| JenkinsBuildInfo {
                        number: b["number"].as_i64().unwrap_or(0),
                        result: b["result"].as_str().map(|s| s.to_string()),
                        timestamp: b["timestamp"].as_i64().unwrap_or(0),
                        duration: b["duration"].as_i64().unwrap_or(0),
                        url: b["url"].as_str().unwrap_or("").to_string(),
                    }))
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
    // Simple URL encoding — avoids pulling in the `urlencoding` crate
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
