# Engine Concurrency Gate Audit (2026-08-11) — 「构建慢」排查完整证据链

## 症状

用户配置 `providers.glmcoding.max_concurrent=32`(opencode-go 底座, deepseek-v4-flash),
执行 `dt build` 全量 65 项目循环, 感觉"慢"。41 分钟只完成 message-center 一小部分。

## 根因

`~/.config/digital-twin/pipeline.yaml` 的 `inference_server.max_concurrent = 1`
——ProcessorEngine(文件级 LLM 分析)并发被限死为 1, 所有文件串行分析。
用户改的 `glmcoding.max_concurrent=32` 只控制 GLM 客户端 semaphore, 被引擎层 1 掐死。

```yaml
# pipeline.yaml (现状)
inference_server:
  url: http://localhost:9997/v1   # xinference 历史遗留, llm_provider=glmcoding 时不使用
  max_concurrent: 1                # ← 真正的引擎并发闸门
providers:
  glmcoding:
    max_concurrent: 32             # ← 用户以为生效的配置(只控制客户端 semaphore)
```

## 代码链路(证据)

| 位置 | 作用 |
|------|------|
| `src/interfaces/cli/build.rs:489` | `ProcessorEngine::new(registry, pipeline_config.inference_server.max_concurrent)` → **引擎并发=1** |
| `src/application/pipeline/engine.rs:130` | `max_concurrent` = "同时运行的 GPU 密集型处理器调用数上限" |
| `src/interfaces/cli/build.rs:339` | glmcoding 分支 `cfg.max_concurrent.unwrap_or(32)` → `GLMCodingChatClient::new` |
| `src/application/pipeline/infer_client.rs:136` | `Semaphore::new(max_concurrent)` — 只限客户端在飞请求上限 |
| `src/interfaces/cli/build.rs:100` | 传入的 max_concurrent 参数在 glmcoding 分支被忽略(仅 xinference 分支用) |

## 吞吐计算(铁证)

- 本次构建 11:38 起, 41 分钟 = 2460s, 完成 215 次 `GLM Coding 响应` → **11.4s/请求完成间隔**
- 200 OK 请求 elapsed_ms p50 = 10.7s → 完成间隔 ≈ 单请求耗时 → **并发 ≈ 1(串行)**
- 若 32 并发真生效: 2460s × 32 / 12s ≈ 6500+ 次, 实际 215 次
- `ss -tnp | grep 784212` 无到 opencode.ai 的出站连接(串行时请求间隙抓不到)

## 排除项(逐一证伪)

1. **时段性限流**: 00:00-02:00 p50=12.2s vs 本次 10.7s — 全天一致, 非时段问题。
   deepseek-v4-flash 长 prompt 代码分析真实负载就是 p50 10-12s / p95 27-29s / max 51s。
   (技能里 2.8s 是 kimi-k3 小请求实测, 不是此负载基准。)
2. **代理**: 构建进程环境带 `http_proxy/socks5=127.0.0.1:7897`(verge-mihomo)。
   curl 对比: 走代理 2.52s vs `--noproxy '*'` 直连 2.34s — 差异可忽略。
3. **429 限流**: 今日 1557 次真 429(提取 `"status":"429` 字段, 非 grep 数字误报)
   集中在 09:00(581)和 11:00(976)时段 = 上午旧构建的 Phase 2 阶段;
   本次 11:38 构建仅开局 22 次, 之后为 0。**慢≠429**。

## 429 统计正确姿势

```python
# 1. 只从日志行提取 status 字段(勿 grep 裸 "429", elapsed_ms 数字会假命中)
out = subprocess.run(['sudo','grep','GLM Coding 响应','/var/log/digital-twin/dt-daemon.log'],
                     capture_output=True, text=True).stdout
lines = [l for l in out.splitlines() if '2026-08-11T' in l]
# 2. 按时间戳过滤本次构建窗口(上午旧构建的 429 会混入)
build = [l for l in lines if l.split('"timestamp":"')[1].split('"')[0] >= '2026-08-11T11:38']
b429 = sum(1 for l in build if '"status":"429' in l)
# 3. 耗时分布
e200 = sorted(int(re.search(r'"elapsed_ms":(\d+)', l).group(1)) for l in build if '"status":"200 OK"' in l)
```

## Phase 2 vs pipeline 文件分析 — 两条并发路径

- **Phase 2 方法分析**(build/pipeline.rs): 直接用 GLM client semaphore → 32 真生效
  → 上午 09:00/11:00 构建的 429 风暴源头(并发吃满触发上游限流)
- **pipeline 文件分析**(ProcessorEngine): 被 inference_server.max_concurrent=1 卡死 → 串行
- 同一构建里可能 Phase 2 429 风暴 + 文件分析串行爬行("冰火两重天")
- 日志区分: `LLM 方法分析开始/完成`(Phase 2) vs `GLM Coding 响应`+`StoreProcessor start`(pipeline)

## 修复(配置改动, 需用户批准 — 用户规矩: 先方案后实施)

1. `pgrep -af 'dt build'` 停残留进程(PID 784212 串行爬行无意义)
2. 两端同步改: `~/.config/digital-twin/pipeline.yaml` + 仓库 `config/pipeline.yaml`
   `inference_server.max_concurrent: 1 → 32`(与 glmcoding 对齐)
3. 重跑构建, 先单项目验证吞吐(预期 20-30 倍提升)
4. 若 32 触发上游 429(Phase 2 曾 32→429 风暴), 降到 16 或 8

## 验证命令集

```bash
pgrep -af 'dt build'                          # 残留进程
ps -o pid,etime,%cpu,stat -p <pid>            # CPU 0% = 纯网络等待
ss -tnp | grep <pid>                          # 在飞连接(443 无 = 串行间隙)
sudo grep 'GLM Coding 响应' /var/log/digital-twin/dt-daemon.log | grep '2026-08-11T' | tail  # 耗时/状态
python3 -c 'import yaml,pathlib;c=yaml.safe_load(pathlib.Path.home()/".config/digital-twin/pipeline.yaml");print(c["inference_server"]["max_concurrent"], c["providers"]["glmcoding"]["max_concurrent"])'
KEY=$(grep -oE 'OPENCODE_GO_API_KEY=.*' ~/.hermes/.env | cut -d= -f2-)
time curl -s --noproxy '*' -o /dev/null -w "HTTP %{http_code} %{time_total}s\n" -X POST https://opencode.ai/zen/go/v1/chat/completions -H "Authorization: Bearer $KEY" -H "Content-Type: application/json" -d '{"model":"deepseek-v4-flash","messages":[{"role":"user","content":"hi"}],"max_tokens":10}'
```
