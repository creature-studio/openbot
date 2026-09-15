//! Spark GPUI Client
//!
//! A native Rust AI agent workbench built on GPUI (Zed's GPU-accelerated UI).
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────────┐
//! │ Spark                                      Connected ●    ⌘K   │
//! ├──────────────┬──────────────────────────────┬───────────────────┤
//! │              │                              │                   │
//! │ BOT          │     修复登录页面              │   Browser         │
//! │ ● Coding Bot │                              │                   │
//! │              │ 用户：                       │  localhost:3000   │
//! │ TASKS        │ 帮我修复登录异常              │                   │
//! │              │                              │  [browser view]   │
//! │ ▶ Login Bug  │ ● 正在工作                   │                   │
//! │ ✓ API Fix    │                              ├───────────────────┤
//! │ ! Deploy     │ ▼ 搜索文件        16ms       │  Files            │
//! │              │   src/login...               │                   │
//! │ WORKBENCH    │                              │  Changes           │
//! │              │ ▼ 读取文件        3ms        │                   │
//! │ Project A    │                              │  Terminal          │
//! │              │ ▼ 修改文件        12ms       │                   │
//! │              │   +12 -3                     │                   │
//! │              │                              │                   │
//! │              │ ▶ cargo test                 │                   │
//! │              │                              │                   │
//! ├──────────────┴──────────────────────────────┴───────────────────┤
//! │  Ask Spark...                                      Stop   Send  │
//! └─────────────────────────────────────────────────────────────────┘
//! ```
