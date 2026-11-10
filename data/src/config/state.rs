use super::ScaleFactor;
use super::sidebar::Sidebar;
use super::timezone::UserTimezone;
use crate::layout::WindowSpec;
use crate::{AudioStream, Layout, Theme};

use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize, Default)]
pub struct Layouts {
    pub layouts: Vec<Layout>,
    pub active_layout: Option<String>,
}

#[derive(Default, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct State {
    pub layout_manager: Layouts,
    pub selected_theme: Theme,
    pub custom_theme: Option<Theme>,
    pub main_window: Option<WindowSpec>,
    pub timezone: UserTimezone,
    pub sidebar: Sidebar,
    pub scale_factor: ScaleFactor,
    pub audio_cfg: AudioStream,
    pub trade_fetch_enabled: bool,
    pub proxy_cfg: Option<exchange::proxy::Proxy>,
}

impl State {
    pub fn from_parts(
        layout_manager: Layouts,
        selected_theme: Theme,
        custom_theme: Option<Theme>,
        main_window: Option<WindowSpec>,
        timezone: UserTimezone,
        sidebar: Sidebar,
        scale_factor: ScaleFactor,
        audio_cfg: AudioStream,
        trade_fetch_enabled: bool,
        proxy_cfg: Option<exchange::proxy::Proxy>,
    ) -> Self {
        State {
            layout_manager,
            selected_theme: Theme(selected_theme.0),
            custom_theme: custom_theme.map(|t| Theme(t.0)),
            main_window,
            timezone,
            sidebar,
            scale_factor,
            audio_cfg,
            trade_fetch_enabled,
            proxy_cfg,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_ignores_fields_left_over_from_removed_features() {
        // `size_in_quote_ccy` backed the crypto-era "size in quote currency" toggle and
        // is gone, but it still sits in every state file written before the removal.
        // Loading must skip it rather than fail, which would reset the user's layouts.
        let legacy = r#"{
            "size_in_quote_ccy": "Quote",
            "trade_fetch_enabled": true
        }"#;

        let state: State = serde_json::from_str(legacy).expect("legacy state still loads");
        assert!(state.trade_fetch_enabled);
    }
}
