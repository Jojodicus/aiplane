// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! How the `/api/v0` agent routes name a refused agent operation.

use rama::http::{Response, StatusCode};

use super::{internal, json_error};
use aiplane_runtime::agents::resume::AgentResumeError;
use aiplane_runtime::suspend::ResumeRefused;

/// A refused resume, in the `/api/v0` envelope. Shared by the staff,
/// inbox and visitor routes, so all name a refusal the same way.
pub(super) fn resume_error(err: AgentResumeError) -> Response {
    let (status, code) = match &err {
        AgentResumeError::Refused(ResumeRefused::NotSuspended)
        | AgentResumeError::Refused(ResumeRefused::StaleRequest { .. }) => {
            (StatusCode::CONFLICT, "not_suspended")
        }
        AgentResumeError::Refused(ResumeRefused::NotOffered { .. }) => {
            (StatusCode::BAD_REQUEST, "decision_not_offered")
        }
        AgentResumeError::StaffOnly { .. } => (StatusCode::FORBIDDEN, "decision_for_staff"),
        AgentResumeError::ParticipantOnly { .. } => (StatusCode::FORBIDDEN, "decision_for_visitor"),
        AgentResumeError::Refused(ResumeRefused::Storage(_)) | AgentResumeError::Db(_) => {
            return internal(err);
        }
    };
    json_error(status, code, &err.to_string())
}
