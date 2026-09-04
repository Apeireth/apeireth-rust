//! Canonical HTTP gateway adapter.
//!
//! The gateway decodes transport requests, invokes the canonical runtime, and
//! encodes responses. It does not own provider routing, tool dispatch,
//! sessions, governance, or a second orchestration engine.

#![deny(unsafe_code)]

/// Native and OpenAI-compatible HTTP chat entry points.
pub mod canonical_entry;

/// Panel introspection surface (`/v1/panel/*`, tools, capabilities, audit, traces).
pub mod panels;

/// Gateway-level SSE event bus (`/v1/apeireth/events`).
pub mod events;

/// Full-duplex real-time voice barge-in and client interrupt controller.
pub mod barge_in;

/// 8-frame full-duplex protocol and streaming sentence divider.
pub mod duplex_gateway;

/// Transparent file fetcher for distributed hyperstack file fetching.
pub mod file_fetcher;

/// Ember HUD 4.0s breath and peripheral vignette glow driver.
pub mod ember_hud_driver;

pub use barge_in::{format_sse_interrupt_event, BargeInController, InterruptReason, StreamHandle};
pub use duplex_gateway::{DuplexFrame, DuplexSessionController, SentenceDivider};
pub use ember_hud_driver::{EmberCognitiveStance, EmberHudDriver, EmberShaderUniforms};
pub use file_fetcher::{
    FetchedFile, FileFetchError, InternalFileRequest, InternalFileResponse, TransparentFileFetcher,
};

pub use canonical_entry::{
    build_gateway_state, build_gateway_state_with_services, canonical_router,
    canonical_router_with_panels, canonical_router_with_services, canonical_router_with_state,
    execute_chat, resolve_approval, serve_canonical, serve_canonical_with_services,
    CanonicalApprovalRequest, CanonicalChatOutcome, CanonicalChatRequest, CanonicalChatResponse,
    CanonicalEntryError, CanonicalExecutionEvent, CanonicalPendingApproval,
};

pub use events::{events_handler, EventBus, GatewayEvent, RuntimeObservationSink};

pub use panels::{
    AuditCommand, AuditDto, AuditQuery, EpisodeDto, EpisodeMutationDto, GatewayServices,
    GatewayState, GrantCommand, GrantDto, GrantMutationDto, GrantQuery, GraphEdgeDto, GraphNodeDto,
    GuardDryRunRequest, GuardDryRunResponse, GuardEventDto, GuardStatusDto, MemoryCommand,
    MemoryGovernanceCommand, MemoryGraphDto, MemoryQuery, ModuleQuery, OrganDto, PanelData,
    SafetyGuardQuery, SessionQuery, SessionSummaryDto, ToolCatalogQuery, ToolDto, TraceCommand,
    TraceDetailDto, TraceQuery, TraceSpanDto, TraceSummaryDto, WorkbenchMemoryProvenanceDto,
    WorkbenchQuery, WorkbenchToolExecutionDto, WorkbenchTurnDto,
};
