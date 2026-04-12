pub mod agent_loop;
pub mod compaction;
pub mod context;
pub mod handle;
pub mod registry;
pub mod subagent;
pub mod team;
pub mod team_lead;
pub mod teammate;
pub mod builder;
pub mod team_tools;
pub mod subagent_tools;
pub mod worktree;

pub use agent_loop::AgentLoop;
pub use subagent::{SubAgentDef, SubAgentRegistry, SubAgentResult, SubAgentRunner};
pub use team::AgentTeam;
pub use team_lead::TeamLead;
