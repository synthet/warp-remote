//! Transport-agnostic session state for the self-hosted remote-control server.
//!
//! The [`Hub`] implements the server half of `session-sharing-protocol`: it owns every live
//! shared session, relays messages between the sharer (a Synth Warp terminal) and its viewers
//! (the web/mobile app or another Warp client), and keeps the ordered event log that late
//! joiners replay. Connections hand it typed messages plus an outbound channel; it never touches
//! sockets, which keeps it unit-testable.

use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use session_sharing_protocol::common::{
    AbsentViewer, ActivePrompt, AddGuestsResponse, BlockId, CommandExecutionFailureReason,
    CommandExecutionRequestId, ControlActionFailureReason, ControlActionRequestId,
    FailedToAddGuestsReason, FailedToRemoveGuestReason, FailedToUpdatePendingUserRoleReason,
    FailedToUpdateTeamAccessLevelReason, InputReplicaId, InputUpdateFailureReason,
    LinkAccessLevelUpdateResponse, OrderedTerminalEvent, OrderedTerminalEventType, ParticipantId,
    ParticipantInfo, ParticipantList, ParticipantPresenceUpdate, PresenceUpdate, PresentViewer,
    ProfileData, RemoveGuestResponse, Role, RoleRequestId, RoleRequestResponse, Scrollback,
    SessionId, SessionSecret, Sharer, TeamAccessLevelUpdateResponse,
    UniversalDeveloperInputContext, UniversalDeveloperInputContextUpdate,
    UpdatePendingUserRoleResponse, Viewer, WindowSize, WriteToPtyFailureReason,
};
use session_sharing_protocol::sharer::{
    self, FailedToInitializeSessionReason, ReconnectPayload, ReconnectToken,
    ReconnectionFailedReason, SessionSourceType, SessionTerminatedReason,
};
use session_sharing_protocol::viewer::{self, FailedToJoinReason, RoleUpdatedReason};
use tokio::sync::mpsc::UnboundedSender;
use tokio::time::Instant;

pub type SharerTx = UnboundedSender<sharer::DownstreamMessage>;
pub type ViewerTx = UnboundedSender<viewer::DownstreamMessage>;

/// Identifies one websocket connection, so a stale connection's disconnect cannot clobber the
/// connection that replaced it (e.g. after a sharer resumes).
pub type ConnId = u64;

#[derive(Clone, Debug)]
pub struct HubConfig {
    /// The participant uid assigned to the sharer and to every authenticated viewer. Using one uid
    /// for both is what lets the sharer honor viewer-driven sizing (phone-sized PTYs).
    pub owner_uid: String,
    pub owner_display_name: String,
    /// Upper bound on scrollback plus event-log bytes kept for one session.
    pub max_session_bytes: u64,
    /// How long a session survives with its sharer disconnected before it is ended.
    pub sharer_reconnect_grace: Duration,
    /// The role given to viewers when they join.
    pub viewer_role: Role,
}

impl Default for HubConfig {
    fn default() -> Self {
        Self {
            owner_uid: "synth-owner".to_owned(),
            owner_display_name: "Owner".to_owned(),
            max_session_bytes: 256 * 1024 * 1024,
            sharer_reconnect_grace: Duration::from_secs(10 * 60),
            viewer_role: Role::Full,
        }
    }
}

/// Client-supplied metadata about the sharer, shown on the dashboard.
#[derive(Clone, Debug, Default)]
pub struct SharerLabels {
    pub host: Option<String>,
}

/// Dashboard row for one live session.
#[derive(Clone, Debug, Serialize)]
pub struct SessionSummary {
    pub id: String,
    pub host: Option<String>,
    pub pwd: Option<String>,
    pub created_at: u64,
    pub last_activity: u64,
    pub sharer_connected: bool,
    pub viewer_count: usize,
    pub command_running: bool,
    pub is_agent: bool,
    pub num_rows: usize,
    pub num_cols: usize,
}

struct ViewerState {
    info: ParticipantInfo,
    role: Role,
    conn: Option<(ConnId, ViewerTx)>,
    supports_full_role: bool,
}

struct Session {
    secret: SessionSecret,
    reconnect_token: ReconnectToken,
    labels: SharerLabels,
    created_at: SystemTime,
    last_activity: SystemTime,
    sharer: ParticipantInfo,
    sharer_conn: Option<(ConnId, SharerTx)>,
    sharer_disconnected_at: Option<Instant>,
    scrollback: Scrollback,
    active_prompt: ActivePrompt,
    initial_window_size: WindowSize,
    latest_window_size: WindowSize,
    init_block_id: BlockId,
    udi_context: Option<UniversalDeveloperInputContext>,
    source_type: SessionSourceType,
    source_task_id: Option<String>,
    events: Vec<OrderedTerminalEvent>,
    num_bytes: u64,
    viewers: BTreeMap<ParticipantId, ViewerState>,
    command_running: bool,
    pwd: Option<String>,
}

pub struct Hub {
    config: HubConfig,
    sessions: Mutex<HashMap<SessionId, Session>>,
    next_conn_id: AtomicU64,
}

impl Hub {
    pub fn new(config: HubConfig) -> Self {
        Self {
            config,
            sessions: Mutex::new(HashMap::new()),
            next_conn_id: AtomicU64::new(1),
        }
    }

    fn sessions(&self) -> std::sync::MutexGuard<'_, HashMap<SessionId, Session>> {
        self.sessions.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn new_conn_id(&self) -> ConnId {
        self.next_conn_id.fetch_add(1, Ordering::Relaxed)
    }

    /// Handles a sharer's `Initialize` on `/sessions/create`.
    pub fn create_session(
        &self,
        init: sharer::InitPayload,
        labels: SharerLabels,
        tx: SharerTx,
    ) -> Option<(SessionId, ConnId)> {
        let scrollback_bytes = init.scrollback.num_bytes().as_u64();
        if scrollback_bytes > self.config.max_session_bytes {
            let _ = tx.send(sharer::DownstreamMessage::FailedToInitializeSession {
                reason: FailedToInitializeSessionReason::ScrollbackTooLarge {},
            });
            return None;
        }

        let session_id = SessionId::new();
        let conn_id = self.new_conn_id();
        let now = SystemTime::now();
        let sharer = ParticipantInfo {
            id: ParticipantId::new(),
            profile_data: ProfileData {
                firebase_uid: self.config.owner_uid.clone(),
                display_name: self.config.owner_display_name.clone(),
                photo_url: None,
                email: None,
                input_replica_id: init.input_replica_id,
            },
            selection: init.selection,
        };
        let session = Session {
            secret: SessionSecret::new(),
            reconnect_token: ReconnectToken::new(),
            labels,
            created_at: now,
            last_activity: now,
            sharer,
            sharer_conn: Some((conn_id, tx)),
            sharer_disconnected_at: None,
            pwd: last_scrollback_pwd(&init.scrollback),
            scrollback: init.scrollback,
            active_prompt: init.active_prompt,
            initial_window_size: init.window_size,
            latest_window_size: init.window_size,
            init_block_id: init.init_block_id,
            udi_context: init.universal_developer_input_context,
            source_type: init.source_type,
            source_task_id: init.source_task_id,
            events: Vec::new(),
            num_bytes: scrollback_bytes,
            viewers: BTreeMap::new(),
            command_running: false,
        };
        session.send_sharer(sharer::DownstreamMessage::SessionInitialized {
            session_id,
            session_secret: session.secret.clone(),
            reconnect_token: session.reconnect_token.clone(),
            sharer_id: session.sharer.id.clone(),
            sharer_firebase_uid: self.config.owner_uid.clone(),
        });
        self.sessions().insert(session_id, session);
        log::info!("Session {session_id} created");
        Some((session_id, conn_id))
    }

    /// Handles a sharer's `Reconnect` on `/sessions/{id}/resume`.
    pub fn resume_session(
        &self,
        session_id: SessionId,
        payload: ReconnectPayload,
        tx: SharerTx,
    ) -> Option<ConnId> {
        let mut sessions = self.sessions();
        let Some(session) = sessions.get_mut(&session_id) else {
            let _ = tx.send(sharer::DownstreamMessage::FailedToReconnect {
                reason: ReconnectionFailedReason::SessionNotFound,
            });
            return None;
        };
        if payload.session_secret != session.secret {
            let _ = tx.send(sharer::DownstreamMessage::FailedToReconnect {
                reason: ReconnectionFailedReason::WrongPassword,
            });
            return None;
        }
        if payload.reconnect_token != session.reconnect_token {
            let _ = tx.send(sharer::DownstreamMessage::FailedToReconnect {
                reason: ReconnectionFailedReason::WrongReconnectionToken,
            });
            return None;
        }

        let conn_id = self.new_conn_id();
        // Replacing the sender drops the previous one, which closes that connection's writer.
        session.sharer_conn = Some((conn_id, tx));
        session.sharer_disconnected_at = None;
        session.sharer.selection = payload.selection;
        session.touch();
        session.send_sharer(sharer::DownstreamMessage::SessionReconnected {
            last_received_event_no: session.events.last().map(|e| e.event_no),
            participant_list: session.participant_list(),
        });
        log::info!("Session {session_id} resumed by sharer");
        Some(conn_id)
    }

    pub fn sharer_disconnected(&self, session_id: SessionId, conn_id: ConnId) {
        let mut sessions = self.sessions();
        if let Some(session) = sessions.get_mut(&session_id)
            && session.is_sharer_conn(conn_id)
        {
            session.sharer_conn = None;
            session.sharer_disconnected_at = Some(Instant::now());
            log::info!("Sharer of session {session_id} disconnected; awaiting resume");
        }
    }

    pub fn handle_sharer_message(
        &self,
        session_id: SessionId,
        conn_id: ConnId,
        message: sharer::UpstreamMessage,
    ) {
        use sharer::{DownstreamMessage as Down, UpstreamMessage as Up};

        let mut sessions = self.sessions();
        let Some(session) = sessions.get_mut(&session_id) else {
            return;
        };
        if !session.is_sharer_conn(conn_id) {
            return;
        }
        session.touch();

        match message {
            Up::Initialize(_) | Up::Reconnect(_) => {
                log::warn!("Ignoring repeated handshake on established sharer connection");
            }
            Up::Ping { data } => session.send_sharer(Down::Pong { data }),
            Up::EndSession { reason } => {
                let reason = match reason {
                    sharer::SessionEndedReason::EndedBySharer => {
                        viewer::SessionEndedReason::EndedBySharer
                    }
                    sharer::SessionEndedReason::InactivityLimitReached => {
                        viewer::SessionEndedReason::InactivityLimitReached
                    }
                    sharer::SessionEndedReason::ExceededSizeLimit => {
                        viewer::SessionEndedReason::ExceededSizeLimit
                    }
                };
                session.broadcast_viewers(viewer::DownstreamMessage::SessionEnded { reason }, None);
                sessions.remove(&session_id);
                log::info!("Session {session_id} ended by sharer");
            }
            Up::ExtendSessionRetention { .. } => {}
            Up::UpdateActivePrompt(update) => {
                session.active_prompt = update.active_prompt.clone();
                session.broadcast_viewers(
                    viewer::DownstreamMessage::ActivePromptUpdated(update),
                    None,
                );
            }
            Up::UpdateUniversalDeveloperInputContext(update) => {
                session.apply_udi_update(&update);
                session.broadcast_viewers(
                    viewer::DownstreamMessage::UniversalDeveloperInputContextUpdated(update),
                    None,
                );
            }
            Up::OrderedTerminalEvent(event) => {
                let latest = session.events.last().map(|e| e.event_no);
                // After a resume, the sharer replays events the server may already hold.
                if latest.is_some_and(|latest| event.event_no <= latest) {
                    session.send_sharer(Down::EventsProcessedAck {
                        latest_processed_event_no: latest.unwrap_or(event.event_no),
                    });
                    return;
                }
                session.num_bytes = session.num_bytes.saturating_add(event.num_bytes().as_u64());
                if session.num_bytes > self.config.max_session_bytes {
                    session.send_sharer(Down::SessionTerminated {
                        reason: SessionTerminatedReason::ExceededSizeLimit,
                    });
                    session.broadcast_viewers(
                        viewer::DownstreamMessage::SessionEnded {
                            reason: viewer::SessionEndedReason::ExceededSizeLimit,
                        },
                        None,
                    );
                    sessions.remove(&session_id);
                    log::info!("Session {session_id} exceeded its size limit");
                    return;
                }
                match &event.event_type {
                    OrderedTerminalEventType::CommandExecutionStarted { .. } => {
                        session.command_running = true;
                    }
                    OrderedTerminalEventType::CommandExecutionFinished { .. } => {
                        session.command_running = false;
                    }
                    OrderedTerminalEventType::Resize { window_size } => {
                        session.latest_window_size = *window_size;
                    }
                    OrderedTerminalEventType::PtyBytesRead { .. }
                    | OrderedTerminalEventType::AgentResponseEvent { .. }
                    | OrderedTerminalEventType::AgentConversationReplayStarted
                    | OrderedTerminalEventType::AgentConversationReplayEnded
                    | OrderedTerminalEventType::CloudModeSetupPhaseEnded => {}
                }
                let event_no = event.event_no;
                session.broadcast_viewers(
                    viewer::DownstreamMessage::OrderedTerminalEvent(event.clone()),
                    None,
                );
                session.events.push(event);
                session.send_sharer(Down::EventsProcessedAck {
                    latest_processed_event_no: event_no,
                });
            }
            Up::UpdateSelection(update) => {
                session.sharer.selection = update.selection.clone();
                let presence = ParticipantPresenceUpdate {
                    participant_id: session.sharer.id.clone(),
                    update: PresenceUpdate::Selection(update.selection),
                };
                session.broadcast_viewers(
                    viewer::DownstreamMessage::ParticipantPresenceUpdated(presence),
                    None,
                );
            }
            Up::UpdateRole {
                participant_id,
                role,
            } => {
                session.set_viewer_roles(
                    |id, _| *id == participant_id,
                    role,
                    RoleUpdatedReason::UpdatedBySharer,
                );
            }
            Up::UpdateUserRole { user_uid, role } => {
                session.set_viewer_roles(
                    |_, info| info.profile_data.firebase_uid == user_uid,
                    role,
                    RoleUpdatedReason::UpdatedBySharer,
                );
            }
            Up::UpdatePendingUserRole { .. } => {
                session.send_sharer(Down::UpdatePendingUserRoleResponse(
                    UpdatePendingUserRoleResponse::Error(
                        FailedToUpdatePendingUserRoleReason::Invalid,
                    ),
                ));
            }
            Up::RespondToRoleRequest {
                participant_id,
                request_id: _,
                response,
            } => {
                if let RoleRequestResponse::Approved { new_role } = &response {
                    session.set_viewer_roles(
                        |id, _| *id == participant_id,
                        *new_role,
                        RoleUpdatedReason::UpdatedBySharer,
                    );
                }
                session.send_viewer(
                    &participant_id,
                    viewer::DownstreamMessage::RoleRequestResponse(response),
                );
            }
            Up::UpdateAllRolesToReader { reason } => {
                session.set_viewer_roles(|_, _| true, Role::Reader, reason.into());
            }
            Up::UpdateInput(update) => {
                session.broadcast_viewers(
                    viewer::DownstreamMessage::InputUpdated(update.clone()),
                    None,
                );
                // The sharer treats its own echoed update as the ack.
                session.send_sharer(Down::InputUpdated(update));
            }
            Up::RejectInputUpdate { id, reason } => {
                session.broadcast_viewers(
                    viewer::DownstreamMessage::InputUpdateRejected {
                        id: id.clone(),
                        reason,
                    },
                    None,
                );
                session.send_sharer(Down::InputUpdateRejectedAck { id });
            }
            Up::RejectCommandExecutionRequest {
                id,
                participant_id,
                reason,
            } => session.send_viewer(
                &participant_id,
                viewer::DownstreamMessage::CommandExecutionRequestFailed { id, reason },
            ),
            Up::RejectWriteToPtyRequest { id, reason } => session.send_viewer(
                &id.participant_id,
                viewer::DownstreamMessage::WriteToPtyRequestFailed { reason },
            ),
            Up::RejectAgentPromptRequest {
                id: _,
                participant_id,
                reason,
            } => session.send_viewer(
                &participant_id,
                viewer::DownstreamMessage::AgentPromptRequestFailed { reason },
            ),
            Up::RejectControlActionRequest {
                participant_id,
                request_id: _,
                reason,
            } => session.send_viewer(
                &participant_id,
                viewer::DownstreamMessage::ControlActionRequestFailed { reason },
            ),
            // Access is governed by the server token, not per-session ACLs, so the ACL editing
            // surface is answered with errors rather than silently ignored.
            Up::UpdateLinkAccessLevel { .. } => {
                session.send_sharer(Down::LinkAccessLevelUpdateResponse(
                    LinkAccessLevelUpdateResponse::Error,
                ));
            }
            Up::UpdateTeamAccessLevel { .. } => {
                session.send_sharer(Down::TeamAccessLevelUpdateResponse(
                    TeamAccessLevelUpdateResponse::Error(
                        FailedToUpdateTeamAccessLevelReason::Invalid,
                    ),
                ));
            }
            Up::AddGuests { .. } => {
                session.send_sharer(Down::AddGuestsResponse(AddGuestsResponse::Error(
                    FailedToAddGuestsReason::Invalid,
                )));
            }
            Up::RemoveGuest { .. } | Up::RemovePendingGuest { .. } => {
                session.send_sharer(Down::RemoveGuestResponse(RemoveGuestResponse::Error(
                    FailedToRemoveGuestReason::Invalid,
                )));
            }
        }
    }

    /// Handles a viewer's `Initialize` on `/sessions/join/{id}`: a first join or a rejoin.
    pub fn join_session(
        &self,
        session_id: SessionId,
        init: viewer::InitPayload,
        display_name: String,
        tx: ViewerTx,
    ) -> Option<(ParticipantId, ConnId)> {
        let mut sessions = self.sessions();
        let Some(session) = sessions.get_mut(&session_id) else {
            let _ = tx.send(viewer::DownstreamMessage::FailedToJoin {
                reason: FailedToJoinReason::SessionNotFound,
            });
            return None;
        };
        let conn_id = self.new_conn_id();
        let supports_full_role = init.feature_support.supports_full_role_for_real;
        session.touch();

        // Rejoin: keep the participant id and replay only what the viewer missed.
        if let Some(viewer_id) = init.viewer_id.as_ref()
            && let Some(existing) = session.viewers.get_mut(viewer_id)
        {
            existing.conn = Some((conn_id, tx));
            existing.supports_full_role = supports_full_role;
            let viewer_id = viewer_id.clone();
            let participant_list = Box::new(session.participant_list());
            session.send_viewer(
                &viewer_id,
                viewer::DownstreamMessage::RejoinedSuccessfully { participant_list },
            );
            session.replay_events(&viewer_id, init.last_received_event_no);
            session.broadcast_participant_list();
            log::info!("Viewer rejoined session {session_id}");
            return Some((viewer_id, conn_id));
        }

        let viewer_id = ParticipantId::new();
        let role = if supports_full_role || self.config.viewer_role != Role::Full {
            self.config.viewer_role
        } else {
            Role::Executor
        };
        let input_replica_id = InputReplicaId::from(uuid::Uuid::new_v4().to_string());
        session.viewers.insert(
            viewer_id.clone(),
            ViewerState {
                info: ParticipantInfo {
                    id: viewer_id.clone(),
                    profile_data: ProfileData {
                        firebase_uid: self.config.owner_uid.clone(),
                        display_name,
                        photo_url: None,
                        email: None,
                        input_replica_id: input_replica_id.clone(),
                    },
                    selection: Default::default(),
                },
                role,
                conn: Some((conn_id, tx)),
                supports_full_role,
            },
        );

        #[allow(deprecated)]
        let joined = viewer::DownstreamMessage::JoinedSuccessfully {
            scrollback: Box::new(session.scrollback.clone()),
            active_prompt: session.active_prompt.clone(),
            latest_event_no: session.events.last().map(|e| e.event_no),
            window_size: session.initial_window_size,
            participant_list: Box::new(session.participant_list()),
            viewer_id: viewer_id.clone(),
            viewer_firebase_uid: self.config.owner_uid.clone(),
            init_block_id: session.init_block_id.clone(),
            input_replica_id,
            universal_developer_input_context: session.udi_context.clone(),
            source_type: (&session.source_type).into(),
            detailed_source_type: session.source_type.clone(),
            source_task_id: session.source_task_id.clone(),
        };
        session.send_viewer(&viewer_id, joined);
        session.replay_events(&viewer_id, None);
        session.broadcast_participant_list();
        log::info!("Viewer joined session {session_id}");
        Some((viewer_id, conn_id))
    }

    pub fn viewer_disconnected(
        &self,
        session_id: SessionId,
        viewer_id: &ParticipantId,
        conn_id: ConnId,
    ) {
        let mut sessions = self.sessions();
        let Some(session) = sessions.get_mut(&session_id) else {
            return;
        };
        let Some(viewer) = session.viewers.get_mut(viewer_id) else {
            return;
        };
        if viewer.conn.as_ref().is_some_and(|(id, _)| *id == conn_id) {
            viewer.conn = None;
            session.broadcast_participant_list();
        }
    }

    pub fn handle_viewer_message(
        &self,
        session_id: SessionId,
        viewer_id: &ParticipantId,
        conn_id: ConnId,
        message: viewer::UpstreamMessage,
    ) {
        use sharer::DownstreamMessage as ToSharer;
        use viewer::{DownstreamMessage as Down, UpstreamMessage as Up};

        let mut sessions = self.sessions();
        let Some(session) = sessions.get_mut(&session_id) else {
            return;
        };
        let Some(viewer) = session.viewers.get_mut(viewer_id) else {
            return;
        };
        if viewer.conn.as_ref().is_none_or(|(id, _)| *id != conn_id) {
            return;
        }
        let can_execute = viewer.role.can_execute();
        let sharer_online = session.sharer_conn.is_some();
        session.touch();

        match message {
            Up::Initialize(_) => log::warn!("Ignoring repeated viewer Initialize"),
            Up::Ping { data } => session.send_viewer(viewer_id, Down::Pong { data }),
            Up::UpdateSelection(update) => {
                if let Some(viewer) = session.viewers.get_mut(viewer_id) {
                    viewer.info.selection = update.selection.clone();
                }
                let presence = ParticipantPresenceUpdate {
                    participant_id: viewer_id.clone(),
                    update: PresenceUpdate::Selection(update.selection),
                };
                session.send_sharer(ToSharer::ParticipantPresenceUpdated(presence.clone()));
                session
                    .broadcast_viewers(Down::ParticipantPresenceUpdated(presence), Some(viewer_id));
            }
            Up::RequestRole(role) => {
                let request_id = RoleRequestId::new();
                session.send_sharer(ToSharer::RoleRequested {
                    participant_id: viewer_id.clone(),
                    request_id: request_id.clone(),
                    role,
                });
                session.send_viewer(viewer_id, Down::RoleRequestInFlight(request_id));
            }
            Up::CancelRoleRequest(request_id) => {
                session.send_sharer(ToSharer::RoleRequestCancelled {
                    participant_id: viewer_id.clone(),
                    request_id,
                });
            }
            Up::UpdateInput(update) => {
                if !can_execute {
                    session.send_viewer(
                        viewer_id,
                        Down::InputUpdateRejected {
                            id: update.id,
                            reason: InputUpdateFailureReason::InsufficientPermissions,
                        },
                    );
                    return;
                }
                session.send_sharer(ToSharer::InputUpdated(update.clone()));
                // Includes the sender, which treats the echo as its ack.
                session.broadcast_viewers(Down::InputUpdated(update), None);
            }
            Up::ExecuteCommand { buffer_id, command } => {
                let id = CommandExecutionRequestId::new();
                let failure = if !can_execute {
                    Some(CommandExecutionFailureReason::InsufficientPermissions)
                } else if !sharer_online {
                    Some(CommandExecutionFailureReason::StaleBuffer)
                } else {
                    None
                };
                if let Some(reason) = failure {
                    session.send_viewer(
                        viewer_id,
                        Down::CommandExecutionRequestFailed { id, reason },
                    );
                    return;
                }
                session.send_viewer(viewer_id, Down::CommandExecutionRequestInFlight(id.clone()));
                session.send_sharer(ToSharer::CommandExecutionRequested {
                    id,
                    participant_id: viewer_id.clone(),
                    buffer_id,
                    command,
                });
            }
            Up::WriteToPty {
                mut request_id,
                bytes,
            } => {
                if !can_execute {
                    session.send_viewer(
                        viewer_id,
                        Down::WriteToPtyRequestFailed {
                            reason: WriteToPtyFailureReason::InsufficientPermissions,
                        },
                    );
                    return;
                }
                // The sharer checks the role of the participant named in the id, so never let a
                // viewer write under someone else's identity.
                request_id.participant_id = viewer_id.clone();
                session.send_sharer(ToSharer::WriteToPtyRequested {
                    id: request_id,
                    bytes,
                });
            }
            Up::SendAgentPrompt(request) => {
                if !can_execute {
                    session.send_viewer(
                        viewer_id,
                        Down::AgentPromptRequestFailed {
                            reason: session_sharing_protocol::common::AgentPromptFailureReason::InsufficientPermissions,
                        },
                    );
                    return;
                }
                session.send_viewer(
                    viewer_id,
                    Down::AgentPromptRequestInFlight(request.id.clone()),
                );
                session.send_sharer(ToSharer::AgentPromptRequested {
                    id: request.id.clone(),
                    participant_id: viewer_id.clone(),
                    request,
                });
            }
            Up::UpdateUniversalDeveloperInputContext(update) => {
                if !can_execute {
                    return;
                }
                session.apply_udi_update(&update);
                session.send_sharer(ToSharer::UniversalDeveloperInputContextUpdated(
                    update.clone(),
                ));
                session.broadcast_viewers(
                    Down::UniversalDeveloperInputContextUpdated(update),
                    Some(viewer_id),
                );
            }
            Up::SendControlAction(action) => {
                let failure = if !can_execute {
                    Some(ControlActionFailureReason::InsufficientPermissions)
                } else if !sharer_online {
                    Some(ControlActionFailureReason::SharerUnavailable)
                } else {
                    None
                };
                if let Some(reason) = failure {
                    session.send_viewer(viewer_id, Down::ControlActionRequestFailed { reason });
                    return;
                }
                session.send_sharer(ToSharer::ControlActionRequested {
                    participant_id: viewer_id.clone(),
                    request_id: ControlActionRequestId::new(),
                    action,
                });
            }
            Up::ReportTerminalSize { window_size } => {
                session.send_sharer(ToSharer::ViewerTerminalSizeReported {
                    participant_id: viewer_id.clone(),
                    window_size,
                });
            }
            Up::Reauthenticated { .. } | Up::UpdateUserRole { .. } => {}
            Up::UpdateLinkAccessLevel { .. } => session.send_viewer(
                viewer_id,
                Down::LinkAccessLevelUpdateResponse(LinkAccessLevelUpdateResponse::Error),
            ),
            Up::UpdateTeamAccessLevel { .. } => session.send_viewer(
                viewer_id,
                Down::TeamAccessLevelUpdateResponse(TeamAccessLevelUpdateResponse::Error(
                    FailedToUpdateTeamAccessLevelReason::Invalid,
                )),
            ),
            Up::AddGuests { .. } => session.send_viewer(
                viewer_id,
                Down::AddGuestsResponse(AddGuestsResponse::Error(FailedToAddGuestsReason::Invalid)),
            ),
            Up::RemoveGuest { .. } | Up::RemovePendingGuest { .. } => session.send_viewer(
                viewer_id,
                Down::RemoveGuestResponse(RemoveGuestResponse::Error(
                    FailedToRemoveGuestReason::Invalid,
                )),
            ),
            Up::UpdatePendingUserRole { .. } => session.send_viewer(
                viewer_id,
                Down::UpdatePendingUserRoleResponse(UpdatePendingUserRoleResponse::Error(
                    FailedToUpdatePendingUserRoleReason::Invalid,
                )),
            ),
        }
    }

    /// Ends sessions whose sharer has been gone longer than the reconnect grace period.
    pub fn reap_abandoned(&self, now: Instant) {
        let grace = self.config.sharer_reconnect_grace;
        self.sessions().retain(|session_id, session| {
            let abandoned = session
                .sharer_disconnected_at
                .is_some_and(|at| now.saturating_duration_since(at) >= grace);
            if abandoned {
                session.broadcast_viewers(
                    viewer::DownstreamMessage::SessionEnded {
                        reason: viewer::SessionEndedReason::InactivityLimitReached,
                    },
                    None,
                );
                log::info!("Session {session_id} ended: sharer did not resume");
            }
            !abandoned
        });
    }

    /// Live sessions for the dashboard, most recently active first.
    pub fn list_sessions(&self) -> Vec<SessionSummary> {
        let mut summaries: Vec<_> = self
            .sessions()
            .iter()
            .map(|(id, session)| SessionSummary {
                id: id.to_string(),
                host: session.labels.host.clone(),
                pwd: session.pwd.clone(),
                created_at: unix_secs(session.created_at),
                last_activity: unix_secs(session.last_activity),
                sharer_connected: session.sharer_conn.is_some(),
                viewer_count: session
                    .viewers
                    .values()
                    .filter(|v| v.conn.is_some())
                    .count(),
                command_running: session.command_running,
                is_agent: matches!(session.source_type, SessionSourceType::AmbientAgent { .. }),
                num_rows: session.latest_window_size.num_rows,
                num_cols: session.latest_window_size.num_cols,
            })
            .collect();
        summaries.sort_by(|a, b| b.last_activity.cmp(&a.last_activity));
        summaries
    }
}

impl Session {
    fn touch(&mut self) {
        self.last_activity = SystemTime::now();
    }

    fn is_sharer_conn(&self, conn_id: ConnId) -> bool {
        self.sharer_conn
            .as_ref()
            .is_some_and(|(id, _)| *id == conn_id)
    }

    fn send_sharer(&self, message: sharer::DownstreamMessage) {
        if let Some((_, tx)) = &self.sharer_conn {
            let _ = tx.send(message);
        }
    }

    fn send_viewer(&self, viewer_id: &ParticipantId, mut message: viewer::DownstreamMessage) {
        let Some(viewer) = self.viewers.get(viewer_id) else {
            return;
        };
        let Some((_, tx)) = &viewer.conn else {
            return;
        };
        if !viewer.supports_full_role {
            message.downgrade_full_roles();
        }
        let _ = tx.send(message);
    }

    fn broadcast_viewers(
        &self,
        message: viewer::DownstreamMessage,
        except: Option<&ParticipantId>,
    ) {
        for viewer_id in self.viewers.keys() {
            if Some(viewer_id) != except {
                self.send_viewer(viewer_id, message.clone());
            }
        }
    }

    fn participant_list(&self) -> ParticipantList {
        let mut list = ParticipantList {
            sharer: Sharer {
                info: self.sharer.clone(),
            },
            ..Default::default()
        };
        for viewer in self.viewers.values() {
            let is_present = viewer.conn.is_some();
            list.viewers.push(Viewer {
                info: viewer.info.clone(),
                role: viewer.role,
                is_present,
            });
            if is_present {
                list.present_viewers.push(PresentViewer {
                    info: viewer.info.clone(),
                    max_acl: viewer.role,
                });
            } else {
                list.absent_viewers.push(AbsentViewer {
                    info: viewer.info.clone(),
                });
            }
        }
        list
    }

    fn broadcast_participant_list(&self) {
        let list = self.participant_list();
        self.send_sharer(sharer::DownstreamMessage::ParticipantListUpdated(
            list.clone(),
        ));
        self.broadcast_viewers(
            viewer::DownstreamMessage::ParticipantListUpdated(list),
            None,
        );
    }

    fn set_viewer_roles(
        &mut self,
        matches: impl Fn(&ParticipantId, &ParticipantInfo) -> bool,
        role: Role,
        reason: RoleUpdatedReason,
    ) {
        let changed: Vec<ParticipantId> = self
            .viewers
            .iter_mut()
            .filter(|(id, viewer)| matches(id, &viewer.info))
            .map(|(id, viewer)| {
                viewer.role = role;
                id.clone()
            })
            .collect();
        if changed.is_empty() {
            return;
        }
        for participant_id in changed {
            self.send_sharer(sharer::DownstreamMessage::ParticipantRoleChanged {
                participant_id: participant_id.clone(),
                role,
            });
            self.broadcast_viewers(
                viewer::DownstreamMessage::ParticipantRoleChanged {
                    participant_id,
                    reason,
                    role,
                },
                None,
            );
        }
        self.broadcast_participant_list();
    }

    fn replay_events(&self, viewer_id: &ParticipantId, after: Option<usize>) {
        for event in &self.events {
            if after.is_none_or(|after| event.event_no > after) {
                self.send_viewer(
                    viewer_id,
                    viewer::DownstreamMessage::OrderedTerminalEvent(event.clone()),
                );
            }
        }
    }

    fn apply_udi_update(&mut self, update: &UniversalDeveloperInputContextUpdate) {
        let context = self.udi_context.get_or_insert_with(Default::default);
        let UniversalDeveloperInputContextUpdate {
            selected_model,
            input_mode,
            selected_conversation,
            long_running_command_agent_interaction_state,
            long_running_command_agent_interaction,
            auto_approve_agent_actions,
            cli_agent_session,
        } = update.clone();
        if selected_model.is_some() {
            context.selected_model = selected_model;
        }
        if input_mode.is_some() {
            context.input_mode = input_mode;
        }
        if selected_conversation.is_some() {
            context.selected_conversation = selected_conversation;
        }
        if long_running_command_agent_interaction_state.is_some() {
            context.long_running_command_agent_interaction_state =
                long_running_command_agent_interaction_state;
        }
        if long_running_command_agent_interaction.is_some() {
            context.long_running_command_agent_interaction = long_running_command_agent_interaction;
        }
        if auto_approve_agent_actions.is_some() {
            context.auto_approve_agent_actions = auto_approve_agent_actions;
        }
        if let Some(cli_agent_session) = cli_agent_session {
            context.cli_agent_session = cli_agent_session;
        }
    }
}

/// Best-effort working directory of the newest scrollback block, for the dashboard. Scrollback
/// blocks are the client's JSON `SerializedBlock`; only its `pwd` field is read.
fn last_scrollback_pwd(scrollback: &Scrollback) -> Option<String> {
    scrollback.blocks.iter().rev().find_map(|block| {
        let value: serde_json::Value = serde_json::from_slice(&block.raw).ok()?;
        value.get("pwd")?.as_str().map(str::to_owned)
    })
}

fn unix_secs(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

#[cfg(test)]
#[path = "hub_tests.rs"]
mod tests;
