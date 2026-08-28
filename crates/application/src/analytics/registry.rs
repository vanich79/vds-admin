//! The analytics provider registry.
//!
//! The single place that knows which providers exist. Registration happens in the
//! composition root, so this module never needs editing when a provider is added.

use std::collections::BTreeMap;
use std::sync::Arc;
use vds_domain::analytics::AnalyticsCapabilities;
use vds_domain::ids::ProviderId;
use vds_domain::ports::AnalyticsProvider;

/// Analytics providers, keyed by id.
#[derive(Default)]
pub struct ProviderRegistry {
    providers: BTreeMap<ProviderId, Arc<dyn AnalyticsProvider>>,
}

impl ProviderRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a provider, replacing any previous one with the same id.
    pub fn register(&mut self, provider: Arc<dyn AnalyticsProvider>) {
        self.providers.insert(provider.id(), provider);
    }

    pub fn get(&self, id: &ProviderId) -> Option<Arc<dyn AnalyticsProvider>> {
        self.providers.get(id).map(Arc::clone)
    }

    pub fn contains(&self, id: &ProviderId) -> bool {
        self.providers.contains_key(id)
    }

    pub fn len(&self) -> usize {
        self.providers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.providers.is_empty()
    }

    /// Every registered provider's id and display name, for the "add integration" form.
    pub fn available(&self) -> Vec<(ProviderId, &'static str)> {
        self.providers
            .values()
            .map(|p| (p.id(), p.display_name()))
            .collect()
    }

    /// Capabilities of one provider, so the UI can hide what it cannot do.
    pub fn capabilities(&self, id: &ProviderId) -> Option<AnalyticsCapabilities> {
        self.providers.get(id).map(|p| p.capabilities())
    }

    pub fn iter(&self) -> impl Iterator<Item = &Arc<dyn AnalyticsProvider>> {
        self.providers.values()
    }
}

impl std::fmt::Debug for ProviderRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderRegistry")
            .field("providers", &self.providers.keys().collect::<Vec<_>>())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use vds_domain::analytics::{AnalyticsInterval, AnalyticsMetric, AnalyticsSnapshot};
    use vds_domain::ids::CredentialRef;
    use vds_domain::ports::{AnalyticsQuery, ProviderError, ProviderHealth};

    struct Stub(&'static str, bool);

    #[async_trait]
    impl AnalyticsProvider for Stub {
        fn id(&self) -> ProviderId {
            ProviderId::new(self.0)
        }

        fn display_name(&self) -> &'static str {
            "Stub provider"
        }

        fn capabilities(&self) -> AnalyticsCapabilities {
            AnalyticsCapabilities {
                supported_metrics: vec![AnalyticsMetric::Visitors],
                supports_time_series: self.1,
                supports_top_pages: false,
                supports_referrers: false,
                supports_realtime: false,
                min_interval: AnalyticsInterval::Day,
                max_history_days: None,
            }
        }

        async fn validate_connection(
            &self,
            _credential_ref: CredentialRef,
        ) -> Result<ProviderHealth, ProviderError> {
            Ok(ProviderHealth::Ok)
        }

        async fn overview(
            &self,
            _query: &AnalyticsQuery,
        ) -> Result<AnalyticsSnapshot, ProviderError> {
            Err(ProviderError::Unsupported("overview"))
        }
    }

    #[test]
    fn a_registered_provider_can_be_looked_up() {
        let mut registry = ProviderRegistry::new();
        assert!(registry.is_empty());

        registry.register(Arc::new(Stub("yandex_metrica", true)));
        assert_eq!(registry.len(), 1);
        assert!(registry.contains(&ProviderId::new("yandex_metrica")));
        assert!(registry.get(&ProviderId::new("yandex_metrica")).is_some());
    }

    #[test]
    fn an_unregistered_provider_is_absent_rather_than_panicking() {
        let registry = ProviderRegistry::new();
        assert!(registry.get(&ProviderId::new("google_analytics")).is_none());
        assert!(registry.capabilities(&ProviderId::new("nope")).is_none());
    }

    #[test]
    fn adding_a_second_provider_does_not_disturb_the_first() {
        // The extensibility property the whole design exists for.
        let mut registry = ProviderRegistry::new();
        registry.register(Arc::new(Stub("yandex_metrica", true)));
        registry.register(Arc::new(Stub("plausible", false)));

        assert_eq!(registry.len(), 2);
        assert!(registry.contains(&ProviderId::new("yandex_metrica")));
        assert!(registry.contains(&ProviderId::new("plausible")));
    }

    #[test]
    fn capabilities_differ_per_provider_so_the_ui_can_adapt() {
        let mut registry = ProviderRegistry::new();
        registry.register(Arc::new(Stub("rich", true)));
        registry.register(Arc::new(Stub("basic", false)));

        assert!(
            registry
                .capabilities(&ProviderId::new("rich"))
                .expect("registered")
                .supports_time_series
        );
        assert!(
            !registry
                .capabilities(&ProviderId::new("basic"))
                .expect("registered")
                .supports_time_series
        );
    }

    #[test]
    fn registering_the_same_id_twice_replaces_rather_than_duplicates() {
        let mut registry = ProviderRegistry::new();
        registry.register(Arc::new(Stub("yandex_metrica", true)));
        registry.register(Arc::new(Stub("yandex_metrica", false)));
        assert_eq!(registry.len(), 1);
    }

    #[test]
    fn the_available_list_drives_the_provider_picker() {
        let mut registry = ProviderRegistry::new();
        registry.register(Arc::new(Stub("yandex_metrica", true)));
        let available = registry.available();
        assert_eq!(available.len(), 1);
        assert_eq!(available[0].0, ProviderId::new("yandex_metrica"));
        assert_eq!(available[0].1, "Stub provider");
    }
}
