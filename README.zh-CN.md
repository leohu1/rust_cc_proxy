# rust_cc_proxy
[English](README.md) | 中文

Rust + actix-web 实现的模块化 Claude Code 代理服务器。支持多供应商路由、协议转换、Token 压缩、用量监控和 cc-switch 兼容。

## 功能特性

- **多供应商路由** — 根据模型名称路由到不同后端（DeepSeek、Anthropic）
- **DeepSeek 兼容** — 三项协议修复（thinking 规范化、thinking 注入、system 角色提取）
- **模型切换 (CC Switch)** — `GET /v1/models` 端点，支持 Claude Code 的 `/model` 选择器
- **自动检测供应商** — 设置 `DEEPSEEK_API_KEY` 即自动启用 DeepSeek，上游地址自动默认
- **Token 压缩** — 内容感知压缩：JSON 数组（BM25 相关性）、JSON 对象、diff、日志、散文
- **CCR (压缩-缓存-检索)** — BLAKE3 哈希 + 内存缓存实现可逆压缩
- **用量监控** — `/v1/usage`、`/metrics`、`/status` 端点，支持流式 token 提取
- **cc-switch 兼容** — `/user/balance`、自定义脚本用量查询
- **开发模式** — 详细请求日志、管线耗时、token 追踪（`--dev` 参数）

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

# 开发模式：详细日志 + /metrics 端点
cargo run -- --dev

# 启用 Token 压缩
COMPRESSION_ENABLED=true DEEPSEEK_API_KEY=sk-... cargo run

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

| 环境变量 | 默认值 | 说明 |
| --- | --- | --- |
| `PROXY_HOST` | `127.0.0.1` | 监听地址 |
| `PROXY_PORT` | `8787` | 监听端口 |
| `PROXY_LOG_LEVEL` | `info` | 日志级别 |
| `PROXY_DEV_MODE` | `false` | 开发模式：详细日志 + `/metrics` |
| `PROXY_UPSTREAM` | `https://api.anthropic.com` | 默认上游地址 |
| `PROXY_API_KEY` | — | 转发到上游的 API 密钥 |
| `PROXY_TIMEOUT` | `600` | 请求超时（秒） |
| `PROXY_POOL_MAX` | `20` | 连接池最大连接数 |
| `PROXY_DUMP_DIR` | — | 流量转储目录（调试用） |
| `COMPRESSION_ENABLED` | `false` | 启用 Token 压缩 |

### DeepSeek 供应商

| 环境变量 | 默认值 | 说明 |
| --- | --- | --- |
| `DEEPSEEK_UPSTREAM` | `https://api.deepseek.com/anthropic` | DeepSeek API 地址（自动默认） |
| `DEEPSEEK_API_KEY` | — | DeepSeek API 密钥（设置即启用） |
| `DEEPSEEK_DEFAULT_MODEL` | `deepseek-v4-flash` | 默认模型 |
| `DEEPSEEK_MODEL_MAP` | — | 模型名称映射：`客户端=上游,...` |

### 命令行参数

```
--host          监听地址（覆盖 PROXY_HOST）
--port          监听端口（覆盖 PROXY_PORT）
--log-level     日志级别（覆盖 PROXY_LOG_LEVEL）
--upstream      默认上游地址（覆盖 PROXY_UPSTREAM）
--dev           启用开发模式（详细日志 + /metrics）
```

## 架构设计

```
Claude Code CLI
  │ POST /v1/messages, GET /v1/models, ...
  ▼
rust_cc_proxy (actix-web)
  ├─ 管线: SystemRoleNormalizer → CompressionStage → ProviderTransform
  ├─ ProviderRegistry → 按模型名称路由（配置 DeepSeek 后自动设为默认）
  └─ ProxyClient (reqwest) → 上游 LLM
```

### 端点

| 路由 | 说明 |
| --- | --- |
| `GET /health` | 健康检查（cc-switch 兼容：`{"status":"healthy"}`） |
| `GET /status` | 代理状态与统计 |
| `GET /v1/usage` | Token 用量查询（cc-switch 兼容） |
| `GET /user/balance` | DeepSeek 格式余额（cc-switch 内置模板） |
| `POST /v1/retrieve` | CCR 内容检索 `{"hash":"..."}` → `{"content":"..."}` |
| `GET /v1/compression/stats` | CCR 缓存统计 |
| `GET /v1/models` | 模型发现（CC Switch） |
| `POST /v1/messages` | 聊天补全（流式 + 非流式） |
| `POST /v1/messages/count_tokens` | Token 计数 |
| `GET /metrics` | 开发模式监控（需要 `--dev` 或 `PROXY_DEV_MODE=true`） |

### 模块结构

```
src/
├── main.rs              命令行入口与服务器启动
├── lib.rs               库入口
├── config.rs            环境变量配置加载
├── error.rs             AppError 错误类型与 HTTP 响应映射
├── monitor/             TokenMonitor: 原子计数器、用量快照
├── server/              actix-web 应用工厂、路由处理、SSE 流
├── pipeline/            PipelineStage trait、system role 规范化
├── providers/           Provider trait、DeepSeek + Anthropic 实现
├── protocol/            Anthropic Messages API 类型、SSE 类型、模型类型
├── proxy/               reqwest 客户端池、SSE 流适配
└── compress/            Token 压缩
    ├── mod.rs           内容检测 + 分发 + token 验证器
    ├── ccr.rs           BLAKE3 哈希 → 内存缓存
    ├── cache_aware.rs   cache_control 检测、冻结区域识别
    ├── live_zone.rs     字节级手术 + SHA-256 前缀完整性
    ├── relevance.rs     BM25 关键词相关性评分
    ├── diff.rs          Unified diff 压缩
    ├── log.rs           构建/测试日志压缩 (pytest/cargo/npm/jest)
    ├── text.rs          提取式散文压缩
    ├── headroom_dll.rs  Headroom DLL 加载器 (LoadLibraryW/dlsym)
    └── pipeline_stage.rs 5 阶段编排器 (检测→哈希→压缩→验证→校验)
```

### Token 压缩

通过 `COMPRESSION_ENABLED=true` 启用。`CompressionStage` 查找最新 user 消息中的 `tool_result` 块，按内容类型压缩：

| 内容类型 | 检测方式 | 策略 |
| --- | --- | --- |
| JSON 数组 | `[...]` + 解析 | BM25 相关性 → 保留首/尾 + 高分中间项 |
| JSON 对象 | `{...}` + 解析 | 字段截断 + CCR |
| Diff | `diff --git` / `@@` 头 | 文件裁剪(5)、块裁剪(3)、上下文修剪(2) |
| Log | 3+ 日志关键词 | 行评分：ERROR(100)、WARN(30)、STACK(50) |
| 散文 | >800 字符 | 句子评分，保留 50% |
| 其他 | — | 跳过（不压缩） |

压缩后内容嵌入 `<<ccr:HASH>>` 标记。可通过 `POST /v1/retrieve {"hash":"..."}` 取回原始数据。

#### 管线编排

`CompressionStage` 中的 5 阶段压缩管线：

```
1. 检测 live zone  →  定位最新 user 消息
2. 缓存安全         →  SHA-256 冻结前缀哈希 + cache_control 检查
3. 内容检测         →  JSON/diff/log/text → 分发到压缩器
4. Token 验证       →  estimate_tokens(压缩后) < estimate_tokens(原始)
5. 完整性校验       →  重新哈希冻结前缀，与原始值比较
```

#### Live-zone 字节手术

`src/compress/live_zone.rs` 对请求体进行字节级手术：
- 对冻结消息（缓存固定前缀）计算 SHA-256 哈希
- 仅压缩最新 user 消息中的 tool_result 块
- 压缩后：验证冻结前缀哈希是否匹配
- 哈希不匹配 → WARN 但保留压缩数据（缓存可重新预热）

#### Token 验证

所有压缩输出通过 token 计数门控（`estimate_tokens()`）：
- ASCII/拉丁：~4 字符/token | 中日韩：~2 字符/token
- 若 `压缩后 tokens >= 原始 tokens` → 拒绝压缩，返回原文
- 防止无效压缩因元数据开销浪费 token

### DeepSeek 兼容性修复

| 修复 | 说明 |
| --- | --- |
| Thinking 规范化 | `adaptive`/`auto` → `enabled`，去除 `reasoning_effort` |
| Thinking 注入 | 在 assistant 消息的 `tool_use` 块前插入空 `thinking` 块 |
| System 角色提取 | 将 `role: "system"` 从 `messages[]` 提取到顶层 `system` 字段 |

## 开发模式

```bash
cargo run -- --dev
```

开发模式启用：
- **详细请求日志**：模型、流模式、管线耗时、token 用量
- **文件+行号** 日志输出
- **`/metrics` 端点**：请求数、token 总量、错误数
- **自动 `debug` 日志级别**（可用 `PROXY_LOG_LEVEL` 覆盖）

日志输出示例：
```
→ REQ  model=deepseek-v4-pro  stream=false  est_input_tokens=3200
  pipeline: 2 stages in 0ms
  provider=DeepSeek  upstream_model=deepseek-v4-pro
← OK   model=deepseek-v4-pro  latency=2100ms  upstream=2050ms  tokens  in=15000  out=800
```

## cc-switch 集成

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
      // DeepSeek 官方 API: total_balance 即为剩余余额 (充值 + 赠送)
      // 所有金额字段为字符串类型, 需 parseFloat 转换
      // 参考: https://api-docs.deepseek.com/zh-cn/api/get-user-balance
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

### 代理链式架构

```
Claude Code → cc-switch (15721) → rust_cc_proxy (8787) → DeepSeek
                 │                        │
                 │ 模型管理                │ 协议修复
                 │ 故障转移                │ Token 压缩
                 │ 用量统计                │ 用量监控
                 │                        │
                 └── GET /v1/usage ──────→┘
```

## 生产加固

### 速率限制
令牌桶算法（10 req/s，突发 20），按供应商分桶。

### 请求体限制
所有路由 20 MB 请求体上限。

### 优雅关闭
SIGTERM / Ctrl+C 时 30 秒排空进行中请求。

### 缓存感知压缩
检测 `cache_control` 标记 — 跳过冻结区域的压缩。

### Headroom DLL 集成

代理启动时按需加载 `headroom_core.dll`，将压缩委托给 Headroom 的生产级算法。DLL 不存在时自动回退到内置轻量压缩器。

### 构建 DLL

```bash
cd D:\projects\headroom
cargo build -p headroom-ffi --release
# 输出: target/release/headroom_ffi.dll
```

### 使用

```bash
cp headroom/target/release/headroom_ffi.dll ./headroom_core.dll
HEADROOM_DLL_PATH=./headroom_core.dll COMPRESSION_ENABLED=true DEEPSEEK_API_KEY=sk-... cargo run
```

启动日志：
```
Loading headroom DLL: ./headroom_core.dll
Headroom DLL loaded successfully
Headroom DLL compression ENABLED
```

### 架构

```
rust_cc_proxy.exe (5 MB)
  ├─ HeadroomDll::load()  ← 按需加载 headroom_core.dll
  │     ├─ compress() → CompressionResult
  │     └─ retrieve() → 原始数据
  └─ 回退: 内置轻量压缩器
```

## Docker

```bash
docker build -t rust_cc_proxy .
docker run -p 8787:8787 -e DEEPSEEK_API_KEY=sk-... rust_cc_proxy
docker run -p 8787:8787 -e DEEPSEEK_API_KEY=sk-... rust_cc_proxy -- --dev
```

## 测试

```bash
cargo test                    # 全部测试（67 个）
cargo test -p rust_cc_proxy   # 单元测试
cargo test --test integration # 集成测试
```

## 实施状态

| 阶段 | 状态 | 说明 |
| --- | --- | --- |
| 1. 基础代理 | ✅ | SSE 透传 + system role 规范化 |
| 2. DeepSeek 供应商 | ✅ | Provider trait、3 项兼容修复 |
| 3. 模型切换 | ✅ | `/v1/models` 端点、模型发现 |
| 4. Token 压缩 | ✅ | JSON/diff/log/text + BM25 + CCR |
| 5. 开发模式与监控 | ✅ | 详细日志、`/metrics`、`/v1/usage` |
| 6. cc-switch 兼容 | ✅ | `/user/balance`、自定义脚本用量查询 |
| 7. 生产加固 | ✅ | 速率限制、优雅关闭、Docker、live-zone 字节手术、管线编排、token 验证 |

## 许可证

MIT
