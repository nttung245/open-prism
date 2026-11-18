//! AI layer of Open Prism: providers, a streaming chat model over `genai`, tools, the patch
//! protocol and the agent loop. The AI never writes files: it proposes patches that the user
//! accepts or rejects.

pub mod agent;
pub mod context;
pub mod error;
pub mod llm;
pub mod mock;
pub mod patch;
pub mod provider;
pub mod search_replace;
pub mod tools;
