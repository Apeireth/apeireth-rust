//! Concrete production modules, kept outside the runtime kernel.

// Kernel ports are re-exported as submodules because the migrated concrete
// implementations use the same `super::module` paths they used before. This
// keeps the move source-compatible without making the kernel depend upward.
pub mod approval {
    pub use apeireth_runtime::canonical::approval::*;
}
pub mod capability {
    pub use apeireth_runtime::canonical::capability::*;
}
pub mod error {
    pub use apeireth_runtime::canonical::error::*;
}
pub mod events {
    pub use apeireth_runtime::canonical::events::*;
}
pub mod execute {
    pub use apeireth_runtime::canonical::execute::*;
}
pub mod module {
    pub use apeireth_runtime::canonical::module::*;
}
pub mod provider {
    pub use apeireth_runtime::canonical::provider::*;
}
pub mod runtime {
    pub use apeireth_runtime::canonical::runtime::*;
}
pub mod session {
    pub use apeireth_runtime::canonical::session::*;
}
pub mod subloop {
    pub use apeireth_runtime::canonical::subloop::*;
}
pub mod trace {
    pub use apeireth_runtime::canonical::trace::*;
}

// Keep the assembly crate pleasant to use for integration tests and host
// composition: kernel ports remain available from the same canonical surface,
// while concrete implementations below stay owned by this crate.
pub use approval::*;
pub use capability::*;
pub use error::*;
pub use events::*;
pub use execute::*;
pub use module::*;
pub use provider::*;
pub use runtime::*;
pub use session::*;
pub use subloop::*;
pub use trace::*;

#[path = "canonical/causal_world_model.rs"]
pub mod causal_world_model;
#[path = "canonical/cognitive.rs"]
pub mod cognitive;
#[path = "canonical/guard_observer.rs"]
pub mod guard_observer;
#[path = "canonical/harness_patch.rs"]
pub mod harness_patch;
#[path = "canonical/orchestrator.rs"]
pub mod orchestrator;
#[path = "canonical/organ_llm_bridge.rs"]
pub mod organ_llm_bridge;
#[path = "canonical/organ_module.rs"]
pub mod organ_module;
#[path = "canonical/preference_learning.rs"]
pub mod preference_learning;
#[path = "canonical/production.rs"]
pub mod production;
#[path = "canonical/tool_modules.rs"]
pub mod tool_modules;
#[path = "canonical/upgrade_cycle.rs"]
pub mod upgrade_cycle;

pub use cognitive::{
    turn_request_from_perception, CognitiveModuleEvent, CognitiveTelemetry, CouncilModule,
    JudgeConfig, JudgeModule, JudgeObservations, JudgeResult, JudgeVerdict, MemoryRecallModule,
    MemoryWritebackModule, ModuleMetricsSnapshot, PreferenceRecallModule, SelfAssessmentModule,
    COUNCIL_MODULE_ID, DEFERRED_COGNITIVE_SLOTS, JUDGE_MODULE_ID, MEMORY_RECALL_MODULE_ID,
    MEMORY_WRITEBACK_MODULE_ID, PREFERENCE_RECALL_MODULE_ID, SELF_ASSESSMENT_MODULE_ID,
};
pub use guard_observer::GuardDatasetObserver;
pub use organ_llm_bridge::{InvokerLlmFactory, InvokerLlmInstance, INVOKER_LLM_FACTORY_NAME};
pub use organ_module::{OrganModule, OrganModuleObservation, ORGAN_MODULE_ID};
pub use preference_learning::{
    PreferenceEvidence, PreferenceLearningModule, PreferenceLearningStats, PreferencePolarity,
    PREFERENCE_LEARNING_MODULE_ID,
};
pub use production::{
    CognitiveBackends, CognitiveModuleConfig, ProductionBackends, ProductionCognitiveModules,
    ProductionModules, ProductionModulesConfig,
};
pub use tool_modules::{
    FetchModule, FilesystemModule, McpModule, RepoModule, SearchModule, ShellModule,
};
