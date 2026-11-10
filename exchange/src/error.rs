use crate::adapter::Venue;

#[derive(thiserror::Error, Debug)]
pub enum AdapterError {
    #[error("Parsing: {0}")]
    ParseError(String),
    #[error("Stream: {0}")]
    WebsocketError(String),
    #[error("Invalid request: {0}")]
    InvalidRequest(String),
    #[error("Adapter unavailable for {venue}: {reason}")]
    Unavailable { venue: Venue, reason: String },
}

impl AdapterError {
    pub fn unavailable(venue: Venue, reason: String) -> Self {
        Self::Unavailable { venue, reason }
    }

    pub fn ui_message(&self) -> String {
        match self {
            Self::ParseError(_) => "Invalid server response. Check logs for details.".to_string(),
            Self::WebsocketError(_) => "Stream error. Check logs for details.".to_string(),
            Self::InvalidRequest(message) => message.clone(),
            Self::Unavailable { reason, .. } => reason.clone(),
        }
    }
}
