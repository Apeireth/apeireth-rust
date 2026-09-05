//! Production composition for [`apeireth_runtime`].
//!
//! This crate owns concrete Memory, Organ, Tool, and SQLite wiring. The
//! runtime crate itself only exposes the lifecycle and port contracts.

#![deny(unsafe_code)]

pub mod canonical;
pub mod sqlite_session;

pub use canonical::{
    CognitiveBackends, CognitiveModuleConfig, CognitiveModuleEvent, CognitiveTelemetry,
    CouncilModule, FetchModule, FilesystemModule, GuardDatasetObserver, InvokerLlmFactory,
    InvokerLlmInstance, JudgeConfig, JudgeModule, JudgeObservations, JudgeResult, JudgeVerdict,
    McpModule, MemoryRecallModule, MemoryWritebackModule, ModuleMetricsSnapshot, OrganModule,
    OrganModuleObservation, PreferenceEvidence, PreferenceLearningModule, PreferenceLearningStats,
    PreferencePolarity, PreferenceRecallModule, ProductionBackends, ProductionCognitiveModules,
    ProductionModules, ProductionModulesConfig, RepoModule, SearchModule, SelfAssessmentModule,
    ShellModule, COUNCIL_MODULE_ID, DEFERRED_COGNITIVE_SLOTS, INVOKER_LLM_FACTORY_NAME,
    JUDGE_MODULE_ID, MEMORY_RECALL_MODULE_ID, MEMORY_WRITEBACK_MODULE_ID, ORGAN_MODULE_ID,
    PREFERENCE_LEARNING_MODULE_ID, PREFERENCE_RECALL_MODULE_ID, SELF_ASSESSMENT_MODULE_ID,
};
pub use sqlite_session::SqliteSessionStore;
