/// Selects whether layout-dependent operations use Moli's deterministic
/// compatibility geometry or construct a real one-shot layout pass.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum LayoutPolicy {
    /// Preserve the pre-layout compatibility behavior and reject operations
    /// such as renderer screenshots that require a real layout backend.
    #[default]
    Mock,
    /// Only screenshot, screencast, and PDF output build layout.
    /// Screenshots and screencasts publish layout; print projections are temporary.
    /// Geometry queries and input read the last published layout, if any.
    OnDemand,
    /// Real layout with Chromium's forced-reflow semantics. Like `OnDemand`,
    /// but a synchronous geometry read, input hit test, `innerText`, or a
    /// layout-dependent computed style lays out again first when the DOM,
    /// style, used-geometry, visual-resource, viewport or media inputs of the
    /// published layout changed. ResizeObserver sampling in the rendering
    /// update does the same. Nothing else lays out without a capture, and an
    /// unchanged Document reuses the published layout.
    Live,
}

impl LayoutPolicy {
    pub const fn uses_real_layout(self) -> bool {
        matches!(self, Self::OnDemand | Self::Live)
    }

    /// Whether synchronous reads relayout when the published layout is stale.
    pub const fn relayouts_stale_reads(self) -> bool {
        matches!(self, Self::Live)
    }
}

/// Immutable browser layout configuration, sealed before its first Page.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct LayoutConfiguration {
    pub policy: LayoutPolicy,
    pub scrollbars_hidden: bool,
}

impl LayoutConfiguration {
    pub const fn scrollbars_hidden_for(self, emulated_hidden: bool) -> bool {
        self.scrollbars_hidden || emulated_hidden
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_policy_defaults_to_mock() {
        assert_eq!(LayoutPolicy::default(), LayoutPolicy::Mock);
        assert!(!LayoutPolicy::default().uses_real_layout());
        assert!(LayoutPolicy::OnDemand.uses_real_layout());
        assert!(!LayoutPolicy::Mock.uses_real_layout());
        assert!(LayoutPolicy::Live.uses_real_layout());
        assert!(LayoutPolicy::Live.relayouts_stale_reads());
        assert!(!LayoutPolicy::OnDemand.relayouts_stale_reads());
    }
}
