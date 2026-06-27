# rust_cc_proxy

[English](README.md) | 中文

Rust + actix-web 实现的生产级 Claude Code 代理服务器。支持多供应商路由、协议转换、自适应 Token 压缩、Prometheus 监控和 API 密钥鉴权。

## 功能特性

- **多供应商路由** — 根据模型名称路由到 DeepSeek 或 Anthropic 后端
- **API 密钥鉴权** — 中间件验证 `x-api-key` 请求头，由 `PROXY_AUTH_TOKENS` 控制
- **DeepSeek 兼容** — 三项协议修复（thinking 规范化、thinking 注入、system 角色提取）
- **模型切换 (CC Switch)** — `/v1/models` 端点 + cc-switch 用量查询支持
- **Token 压缩** — 内容感知压缩：JSON 数组、diff、日志、搜索、散文，带自适应大小（Kneedle 算法）、锚点选择和预压缩优化
- **统一信号评分** — `LineImportanceDetector` trait + `KeywordDetector`，上下文感知关键词激活，覆盖所有压缩器
- **CCR (压缩-缓存-检索)** — BLAKE3 哈希 + InMemory (LRU) 或 SQLite (WAL、TTL、后台清理) 可逆压缩
- **Prometheus 监控** — `/metrics` 端点（计数器、直方图、仪表盘），始终开启
- **用量监控** — `/v1/usage`、`/status` 端点，SSE 拦截提取流式 token
- **Headroom DLL** — 可选动态加载 `headroom_core.dll`，生产级压缩算法
- **生产加固** — 速率限制、20 MB 请求体上限、30 秒优雅关闭、缓存感知 live-zone 手术

## 快速开始

### 前置条件

- Rust 1.80+（edition 2021）
- DeepSeek API 密钥（或 Anthropic 密钥用于直通模式）

### 一键安装

```bash
# Linux / macOS
curl -fsSL https://raw.githubusercontent.com/leohu1/rust_cc_proxy/master/install.sh | bash

# Windows (PowerShell 管理员)
iwr -Uri https://raw.githubusercontent.com/leohu1/rust_cc_proxy/master/install.ps1 -OutFile install.ps1
powershell -ExecutionPolicy Bypass -File install.ps1
```

### 从源码构建

```bash
git clone https://github.com/leohu1/rust_cc_proxy.git
cd rust_cc_proxy
cargo build --release                       # 仅代理
cargo build -p headroom-ffi --release        # 可选压缩 DLL
```

### 运行

```bash
# DeepSeek 后端 — 设置 DEEPSEEK_API_KEY 即自动启用
DEEPSEEK_API_KEY=sk-your-deepseek-key cargo run

# 启用鉴权
PROXY_AUTH_TOKENS=sk-proxy-key-1,sk-proxy-key-2 DEEPSEEK_API_KEY=sk-... cargo run

# 启用 Token 压缩
COMPRESSION_ENABLED=true DEEPSEEK_API_KEY=sk-... cargo run

# SQLite CCR 持久化
COMPRESSION_ENABLED=true CCR_BACKEND=sqlite CCR_SQLITE_PATH=ccr.db cargo run

# 自定义端口
PROXY_PORT=8787 DEEPSEEK_API_KEY=sk-... cargo run
```

### 配合 Claude Code 使用

```bash
ANTHROPIC_BASE_URL=http://localhost:8787 \
ANTHROPIC_API_KEY="" \
ANTHROPIC_AUTH_TOKEN=any-value \
CLAUDE_CODE_ATTRIBUTION_HEADER=0 \
CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY=1 \
claude
```

在 Claude Code 中输入 `/model` 即可切换模型。

## 配置说明

优先级：命令行参数 > 环境变量 > 默认值。

### 核心配置

| 环境变量 | 默认值 | 说明 |
| --- | --- | --- |
| `PROXY_HOST` | `127.0.0.1` | 监听地址 |
| `PROXY_PORT` | `8787` | 监听端口 |
| `PROXY_LOG_LEVEL` | `info` | 日志级别 |
| `PROXY_DEV_MODE` | `false` | 开发模式：详细日志 + `/v1/metrics` |
| `PROXY_UPSTREAM` | `https://api.anthropic.com` | 默认上游地址 |
| `PROXY_API_KEY` | — | 转发到上游的 API 密钥 |
| `PROXY_TIMEOUT` | `600` | 请求超时（秒） |
| `PROXY_POOL_MAX` | `20` | 连接池最大连接数 |
| `PROXY_DUMP_DIR` | — | 流量转储目录 |

### 鉴权

| 环境变量 | 默认值 | 说明 |
| --- | --- | --- |
| `PROXY_AUTH_TOKENS` | — | 逗号分隔的 API 密钥。空=不启用鉴权。客户端需设置 `x-api-key` 请求头。`/health` 始终开放。 |

### DeepSeek 供应商

| 环境变量 | 默认值 | 说明 |
| --- | --- | --- |
| `DEEPSEEK_UPSTREAM` | `https://api.deepseek.com/anthropic` | DeepSeek API 地址 |
| `DEEPSEEK_API_KEY` | — | 设置即自动启用 DeepSeek |
| `DEEPSEEK_DEFAULT_MODEL` | `deepseek-v4-flash` | 默认模型 |
| `DEEPSEEK_MODEL_MAP` | — | 模型名称映射：`客户端=上游,...` |

### 压缩

| 环境变量 | 默认值 | 说明 |
| --- | --- | --- |
| `COMPRESSION_ENABLED` | `false` | 启用 Token 压缩 |
| `CCR_BACKEND` | `memory` | CCR 存储：`memory` (LRU) 或 `sqlite` (持久化) |
| `CCR_SQLITE_PATH` | — | SQLite 数据库路径（backend = `sqlite` 时） |
| `CCR_TTL_SECONDS` | `1800` | 缓存条目 TTL 秒数（0 = 永不过期） |
| `CCR_PURGE_INTERVAL_SECONDS` | `300` | 后台 TTL 清理间隔（仅 SQLite；0 = 禁用） |

### 命令行参数

```
--host          监听地址（覆盖 PROXY_HOST）
--port          监听端口（覆盖 PROXY_PORT）
--log-level     日志级别（覆盖 PROXY_LOG_LEVEL）
--upstream      默认上游地址（覆盖 PROXY_UPSTREAM）
--dev           启用开发模式（详细日志 + /v1/metrics）
```

## 架构设计

```
Claude Code CLI
  │ POST /v1/messages, GET /v1/models, ...
  ▼
rust_cc_proxy (actix-web)
  ├─ Auth 中间件              → x-api-key 验证（可选）
  ├─ Pipeline                 → SystemRoleNormalizer → CompressionStage → ProviderTransform
  ├─ ProviderRegistry         → 按模型名称路由（配置 DeepSeek 后自动设为默认）
  ├─ ProxyClient (reqwest)    → 上游 LLM
  └─ Prometheus /metrics      → 计数器、直方图、仪表盘（始终开启）
```

### 端点

| 路由 | 鉴权 | 说明 |
| --- | --- | --- |
| `GET /health` | 否 | 健康检查：`{"status":"healthy"}` |
| `GET /metrics` | 是 | **Prometheus 文本格式**（始终开启） |
| `GET /status` | 是 | 代理状态与用量统计 |
| `GET /v1/usage` | 是 | Token 用量查询（cc-switch 兼容） |
| `GET /user/balance` | 是 | DeepSeek 格式余额 |
| `POST /v1/retrieve` | 是 | CCR 内容检索 `{"hash":"..."}` → `{"content":"..."}` |
| `GET /v1/compression/stats` | 是 | CCR 缓存统计 |
| `GET /v1/models` | 是 | 模型发现（CC Switch） |
| `POST /v1/messages` | 是 | 聊天补全（流式 + 非流式） |
| `POST /v1/messages/count_tokens` | 是 | Token 计数 |
| `GET /v1/metrics` | 是 | 开发模式 JSON 指标（需 `PROXY_DEV_MODE=true`） |

### 模块结构

```
src/
├── main.rs                   CLI 入口 + 服务器启动
├── lib.rs                    模块声明
├── auth.rs                   API 密钥中间件 (x-api-key)
├── config.rs                 环境变量配置加载
├── error.rs                  AppError 错误类型 + HTTP 响应映射
├── metrics.rs                Prometheus 指标（计数器、直方图、仪表盘）
├── monitor/                  TokenMonitor: 原子计数器、用量/SSE 解析
├── server/
│   ├── mod.rs                actix-web 应用工厂、路由装配
│   ├── handlers.rs           路由处理器（10 个端点）
│   ├── rate_limiter.rs       令牌桶速率限制器（按供应商分桶）
│   └── shutdown.rs           SIGTERM / Ctrl+C 优雅关闭
├── pipeline/
│   ├── mod.rs                PipelineStage trait + Pipeline 运行器
│   └── system_normalizer.rs  System 角色提取/合并
├── providers/
│   ├── mod.rs                Provider trait + ProviderRegistry
│   ├── deepseek.rs           DeepSeek 协议修复（thinking、system role）
│   └── anthropic.rs          Anthropic 直通
├── protocol/
│   ├── mod.rs                协议类型重导出
│   ├── messages.rs           Anthropic Messages API 类型
│   ├── models.rs             模型列表类型
│   └── sse_types.rs          SSE 事件类型 + 流式解析
├── proxy/
│   ├── mod.rs                reqwest 客户端池、转发助手
│   └── streaming.rs          SSE 流透传 + token 提取
└── compress/
    ├── mod.rs                内容类型检测、Compressor 分发器、token 门控
    ├── pipeline_stage.rs     CompressionStage: 5 阶段编排器
    ├── cache_aware.rs        cache_control 检测、冻结区域识别
    ├── live_zone.rs          字节级手术 + SHA-256 前缀完整性
    ├── headroom_dll.rs       Headroom DLL 加载器 (LoadLibraryW/dlsym)
    ├── tokenizer.rs          tiktoken-rs (o200k_base) + 字符估算回退
    ├── signals.rs            LineImportanceDetector trait + KeywordDetector（统一评分）
    ├── pipeline_utils.rs     JsonMinify + DiffNoise 预压缩优化 + 膨胀检测
    ├── adaptive_sizer.rs     Kneedle 最优保留数量算法
    ├── anchor_selector.rs    加权三区锚点分配
    ├── relevance.rs          BM25 关键词相关性评分
    ├── diff.rs               Unified diff 压缩器
    ├── log.rs                构建/测试日志压缩器 (pytest/cargo/npm/jest)
    ├── search.rs             grep/ripgrep 搜索结果压缩器
    ├── text.rs               提取式散文压缩器
    └── ccr/
        ├── mod.rs            CcrBackend trait + CcrStore 包装器 + BLAKE3 哈希 + 统计
        ├── memory.rs         InMemory 后端（LRU 淘汰）
        └── sqlite.rs         SQLite 后端（WAL、TTL 过期、后台清理）
```

### Token 压缩管线

通过 `COMPRESSION_ENABLED=true` 启用。`CompressionStage` 定位最新 user 消息并压缩 `tool_result` 块：

```
1. 预压缩优化    →  JsonMinify（无损压缩） + DiffNoise（去除 lockfile/纯空白）
2. 内容类型检测  →  JSON 数组/对象、diff、日志、搜索结果、纯文本
3. 信号评分      →  LineImportanceDetector::score() — 关键词 + 结构加成
4. 自适应大小    →  Kneedle 在 bigram 覆盖曲线上查找最佳保留数
5. 锚点选择      →  根据数据模式分配前/中/后三区锚点预算
6. 压缩器分发    →  按内容类型选择压缩器 + 填充
7. Token 门控    →  tiktoken-rs (o200k_base) → 无效压缩则拒绝
8. CCR 存储      →  BLAKE3 哈希 → 存储原文 → 嵌入 <<ccr:HASH>> 标记
```

#### 内容类型压缩策略

| 类型 | 检测方式 | 策略 |
| --- | --- | --- |
| JSON 数组 | `[...]` + 解析 | AdaptiveSizer → AnchorSelector → BM25 填充 → CCR |
| JSON 对象 | `{...}` + 解析 | 字段截断 + CCR |
| Diff | `diff --git` / `@@` | 去噪 → 文件裁剪(5) → 块裁剪(3) → CCR |
| Log | 3+ 日志关键词 | 统一 KeywordDetector 评分（Error=0.9, Warning=0.6）+ 上下文窗口 |
| 搜索 | `文件:数字:` 模式 | 首个分隔符解析器 → 自适应文件/匹配数量 → CCR |
| 散文 | >800 字符 | 句子评分（Error/Warning 信号 + 新近度 + 密度）→ 保留上半部分 |
| 其他 | — | 跳过（保持不变） |

压缩后内容嵌入 `<<ccr:HASH>>` 标记。通过 `POST /v1/retrieve {"hash":"..."}` 取回原始数据。

#### 自适应大小（Kneedle 算法）

用数据驱动决策替代硬编码的"保留 N 项"：
- 按原始顺序计算累计唯一 bigram 覆盖曲线
- 找到曲线的拐点（肘部）—— 与 y=x 对角线最大垂直距离
- 应用偏差乘数，限制在 `[min_k, max_k]` 范围内
- 适用于 JSON 数组（项数）和搜索结果（匹配数）

#### 信号框架

`LineImportanceDetector` trait 统一所有行级评分：
- **KeywordDetector**：Error (0.9)、Security (0.85)、Warning (0.6)、Importance (0.45)
- **上下文感知**：Warning 在 Diff 中禁用、Security 仅 Diff 生效、Error 全场景生效
- **结构加成**：全大写词（每个 +0.03）、数字密度（+0.1）
- **词边界匹配**：`"error"` 匹配 `"error:"` 但不匹配 `"terrorism"`

### CCR 存储后端

| 后端 | 淘汰策略 | 持久化 | 适用场景 |
| --- | --- | --- | --- |
| InMemory | LRU | 否 | 开发、测试、小规模 |
| SQLite | TTL + 后台清理 | 是 | 生产单实例 |

SQLite 后端特性：WAL 模式、`synchronous=NORMAL`、`busy_timeout=5000`、启动时逾期清理、可选后台 `tokio` 定时清理任务。

### DeepSeek 兼容性修复

| 修复 | 说明 |
| --- | --- |
| Thinking 规范化 | `adaptive`/`auto` → `enabled`，去除 `reasoning_effort` |
| Thinking 注入 | 在 assistant 消息的 `tool_use` 块前插入空 `thinking` 块 |
| System 角色提取 | 将 `role: "system"` 从 `messages[]` 提取到顶层 `system` 字段 |

## 开发模式

```bash
cargo run -- --dev
# 或: PROXY_DEV_MODE=true cargo run
```

开发模式启用：
- **详细请求日志**：模型、流模式、管线耗时、token 用量
- **文件+行号** 日志输出
- **`/v1/metrics` 端点**：JSON 格式的请求数、token 总量、错误数
- **自动 `debug` 日志级别**（可用 `PROXY_LOG_LEVEL` 覆盖）

日志输出示例：
```
→ REQ  model=deepseek-v4-pro  stream=false  est_input_tokens=3200
  pipeline: 2 stages in 0ms
  provider=DeepSeek  upstream_model=deepseek-v4-pro
← OK   model=deepseek-v4-pro  latency=2100ms  upstream=2050ms  tokens  in=15000  out=800
```

## cc-switch 集成

### 代理链式架构

```
Claude Code → cc-switch (15721) → rust_cc_proxy (8787) → DeepSeek / Anthropic
                 │                        │
                 │ 模型管理                │ 协议修复
                 │ 故障转移                │ Token 压缩
                 │ 用量统计                │ Prometheus 指标
                 │                        │
                 └── GET /v1/usage ──────→┘
```

### 用量查询自定义脚本

在 cc-switch 中选择 **"自定义 (Custom)"** 模板：

```javascript
({
  request: {
    url: "{{baseUrl}}/user/balance",
    method: "GET",
    headers: { "Authorization": "Bearer {{apiKey}}" }
  },
  extractor: function(response) {
    if (response.balance_infos && response.balance_infos.length > 0) {
      var info = response.balance_infos[0];
      var total  = parseFloat(info.total_balance)  || 0;
      var topped = parseFloat(info.topped_up_balance) || 0;
      var granted = parseFloat(info.granted_balance) || 0;
      return {
        planName: "DeepSeek",
        remaining: total,
        used: Math.max(0, topped + granted - total),
        total: topped + granted,
        unit: info.currency || "CNY",
        isValid: response.is_available
      };
    }
    return { isValid: false, invalidMessage: "No data" };
  }
})
```

## 生产加固

### 速率限制
令牌桶算法（10 req/s，突发 20），按供应商分桶。

### 请求体限制
所有路由 20 MB 请求体上限。

### 优雅关闭
SIGTERM / Ctrl+C 时 30 秒排空进行中请求。

### 缓存感知压缩
检测 `cache_control` 标记 — 跳过冻结区域的压缩。Live-zone 字节级手术配合 SHA-256 完整性验证。

### Headroom DLL 集成

代理启动时按需加载 `headroom_core.dll`，将压缩委托给 Headroom 的生产级算法。DLL 不存在时自动回退到内置轻量压缩器。

```bash
# 构建
cd D:\projects\headroom
cargo build -p headroom-ffi --release

# 使用
cp target/release/headroom_ffi.dll ./headroom_core.dll
HEADROOM_DLL_PATH=./headroom_core.dll COMPRESSION_ENABLED=true DEEPSEEK_API_KEY=sk-... cargo run
```

## Docker

```bash
docker build -t rust_cc_proxy .
docker run -p 8787:8787 -e DEEPSEEK_API_KEY=sk-... rust_cc_proxy
docker run -p 8787:8787 -e DEEPSEEK_API_KEY=sk-... -e PROXY_AUTH_TOKENS=my-key rust_cc_proxy
docker run -p 8787:8787 -e DEEPSEEK_API_KEY=sk-... rust_cc_proxy -- --dev
```

## 测试

```bash
cargo test                    # 全部测试（137+ 个）
cargo test --lib              # 单元测试
cargo test --test integration # 集成测试
COMPRESSION_ENABLED=1 CCR_BACKEND=memory cargo test --lib  # 快速检查
```

## 实施状态

| 阶段 | 说明 | 状态 |
| --- | --- | --- |
| 基础 | SSE 透传、system role 规范化、DeepSeek 协议修复 | ✅ |
| CC Switch | `/v1/models`、模型发现、cc-switch 用量/余额端点 | ✅ |
| 压缩 | 6 种内容类型压缩器、BM25、CCR、tiktoken-rs 分词器 | ✅ |
| 第1批 | SQLite CCR、SearchCompressor、tiktoken-rs、CcrBackend trait、headroom_ccr_stats FFI | ✅ |
| 第2批 | LineImportanceDetector、AdaptiveSizer、AnchorSelector、Pipeline 预压缩优化 | ✅ |
| 第3批 | Auth 中间件、LRU 缓存加固、Prometheus 指标、服务器接线 | ✅ |
| 生产 | 速率限制、优雅关闭、Docker、live-zone 手术、缓存感知 | ✅ |

## 许可证

MIT
