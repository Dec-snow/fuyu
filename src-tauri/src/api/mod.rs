pub mod complete;
pub mod image;
pub mod openai;
pub mod sse;
pub mod stream_client;

/// 一次外部请求允许读取的最大字节数。每一处外部输入都要有上限，
/// 否则一个坏掉/恶意的服务端就能把进程内存吃光。
pub const MAX_ERROR_BODY: usize = 128 * 1024; // 128 KiB
pub const MAX_JSON_BODY: usize = 2 * 1024 * 1024; // 2 MiB
