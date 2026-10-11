//! Agent 工具系统。
//!
//! 与参考项目一致，这里**没有 `Tool` trait**：一个工具就是「一段描述 +
//! 一个执行函数」，用 `match` 分发。trait 抽象在这里只会带来 `Box<dyn>` 和
//! 生命周期地狱，而收益（多态替换）当前为零。

pub mod registry;
pub mod web;
