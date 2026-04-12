pub mod agent_loop;
pub mod agent_tool;
pub mod compaction;
pub mod subagent;
pub mod team;
pub mod builder;
pub mod worktree;

pub use agent_loop::AgentLoop;
pub use subagent::{SubAgentDef, SubAgentRegistry, SubAgentResult, SubAgentRunner};
pub use team::AgentTeam;
