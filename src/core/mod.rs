//! 协议内核：规范表示（IR）与各协议适配器，无 HTTP 依赖。
//!
//! `ir` 是唯一中枢，`openai_chat` 等适配器在 wire 与 IR
//! 之间双向编解码，wire 类型不出适配器边界。

pub mod anthropic_messages;
pub mod billing;
pub mod gemini;
pub mod ir;
pub mod openai_chat;
pub mod openai_responses;
pub mod stream;

#[cfg(test)]
mod roundtrip;

#[cfg(test)]
pub(crate) mod testing;
